// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
// METAPLEXIS-DISCLAIMER-END

 
 
 
 
 
 
 
 
 
 
 
 
 
(() => {
  "use strict";

  const PANELS = [
    {
      id: "dynamic-results",
      action: "inspect_dynamic_results",
      launcher: "Dynamic results…",
      title: "Dynamic results",
      tooltip: "Play the frames and time series captured by dynamic evaluations and optimisation jobs.",
      help: "Frames, deformed bodies and time series captured by dynamic evaluations and optimisation jobs, rendered by the service from the stored provider fields (the same renders as the agent actions).",
      src: "rust_ext/dynamic_results.html?embedded=1",
      global: "ImplexityDynamicResults",
    },
  ];
  const state = { served: new Set(), known: false, probe: 0, launchers: new Map(), dialogs: new Map() };

  const make = (tag, text, className) => {
    const node = document.createElement(tag);
    if (text) node.textContent = text;
    if (className) node.className = className;
    return node;
  };

  function stylesheet() {
    if (document.querySelector("link[data-implexity-rust-extension]")) return;
    const link = make("link");
    link.rel = "stylesheet";
    link.href = "rust_ext/extensions.css";
    link.dataset.implexityRustExtension = "";
    document.head.append(link);
  }

  async function probe() {
    const serial = ++state.probe;
    let body = null;
    try {
      const response = await fetch("/v1/agent/capabilities", { cache: "no-store", credentials: "same-origin" });
      if (!response.ok) return;
      body = await response.json();
    } catch (e) {
      return;
    }
    if (serial !== state.probe) return;
    const actions = body && body.rust_extension && Array.isArray(body.rust_extension.actions) ? body.rust_extension.actions : [];
    state.served = new Set(actions.map((a) => a && a.name).filter(Boolean));
    state.known = true;
    sync();
  }

  function toolbar() {
    return document.getElementById("implexityTopbarCommands") || document.querySelector("#topbar .implexity-topbar-commands");
  }

  function dialogFor(panel) {
    let entry = state.dialogs.get(panel.id);
    if (entry) return entry;
    const dialog = make("dialog", "", "advanced-commands implexity-rust-panel-dialog");
    dialog.id = `implexity-rust-panel-${panel.id}`;
    const titleId = `${dialog.id}-title`;
    dialog.setAttribute("aria-labelledby", titleId);
    const head = make("header");
    const title = make("h2", panel.title);
    title.id = titleId;
    const close = make("button", "Close");
    close.type = "button";
    close.addEventListener("click", () => dialog.close());
    head.append(title, close);
    const help = make("p", panel.help, "advanced-help");
    const body = make("div", "", "implexity-rust-panel-body");
    const notice = make("p", "", "implexity-rust-panel-unavailable");
    notice.setAttribute("role", "status");
    notice.hidden = true;
    body.append(notice);
    dialog.append(head, help, body);
    dialog.addEventListener("close", () => entry.returnFocus?.focus?.({ preventScroll: true }));
    document.body.append(dialog);
    entry = { dialog, body, notice, frame: null, returnFocus: null };
    state.dialogs.set(panel.id, entry);
    return entry;
  }

  function open(panel) {
    const entry = dialogFor(panel);
    if (!entry.frame) {
       
      const frame = make("iframe", "", "implexity-rust-panel-frame");
      frame.title = panel.title;
      frame.src = panel.src;
      entry.body.append(frame);
      entry.frame = frame;
    }
    entry.notice.hidden = true;
    entry.frame.hidden = false;
    if (!entry.dialog.open) {
      entry.returnFocus = document.activeElement;
      entry.dialog.showModal();
    }
  }

   
  function withdraw(panel) {
    const entry = state.dialogs.get(panel.id);
    if (!entry) return;
    if (entry.frame) {
      try {
        entry.frame.contentWindow?.[panel.global]?.setAvailable?.(false);
      } catch (e) {
         
      }
      entry.frame.remove();
      entry.frame = null;
    }
    entry.notice.textContent = `${panel.title} are not available now: no active physics package captures them. Activate a dynamic physics package under Physics add-ins to use this panel again.`;
    entry.notice.hidden = false;
  }

  function sync() {
    const bar = toolbar();
    for (const panel of PANELS) {
      const on = state.known && state.served.has(panel.action);
      let button = state.launchers.get(panel.id);
      if (on && !button && bar) {
        button = make("button", panel.launcher);
        button.type = "button";
        button.id = `implexity-rust-panel-open-${panel.id}`;
        button.title = panel.tooltip;
        button.dataset.implexityExtensionPanel = panel.id;
        button.addEventListener("click", () => open(panel));
        bar.append(button);
        state.launchers.set(panel.id, button);
      }
      if (button) button.hidden = !on;
      if (!on && state.known) withdraw(panel);
    }
  }

  function whenToolbar() {
    if (toolbar()) {
      sync();
      return;
    }
    const observer = new MutationObserver(() => {
      if (toolbar()) {
        observer.disconnect();
        sync();
      }
    });
    observer.observe(document.body, { childList: true, subtree: true });
  }

  function boot() {
    stylesheet();
    whenToolbar();
    probe();
    window.addEventListener("implexity:physics-packages-changed", () => probe());
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState === "visible") probe();
    });
    setInterval(() => {
      if (document.visibilityState === "visible") probe();
    }, 60000);
  }

  window.ImplexityRustExtension = { panels: PANELS.map((p) => p.id), open: (id) => { const p = PANELS.find((x) => x.id === id); if (p) open(p); }, probe };
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", boot, { once: true });
  else boot();
})();
