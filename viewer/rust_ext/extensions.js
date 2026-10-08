// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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
