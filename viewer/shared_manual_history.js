// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

(function (global) {
  "use strict";
  function createHistory({read, changed = () => {}}) {
    const owners = new Map();
    let state = null, applying = false, needsRefresh = false, error = null;
    let requestSerial = 0, pendingReads = 0;
    const counts = () => ({
      undo: error ? 0 : state?.undo || 0, redo: error ? 0 : state?.redo || 0,
      nextUndo: state?.undo_head?.label || null, nextRedo: state?.redo_head?.label || null,
      busy: applying || needsRefresh, needsRefresh, error,
      revision: state?.revision || null, stale: Boolean(state?.stale),
      externalBusy: Boolean(state?.active_transaction),
    });
    const publish = () => changed(counts());
    function adopt(value) {
      if (!value || value.schema !== "implexity-manual-command-history/1" ||
          typeof value.revision !== "string" ||
          !Number.isSafeInteger(value.undo) || value.undo < 0 || !Number.isSafeInteger(value.redo) || value.redo < 0)
        throw new Error("The service did not return a valid shared command history.");
      state = structuredClone(value); error = null; publish();
    }
    async function refresh() {
      const id = ++requestSerial; pendingReads++;
      try {
        const value = await read();
        if (id === requestSerial) adopt(value);
        return value;
      } catch (failure) {
        if (id === requestSerial) {error = failure.message; publish();}
        throw failure;
      } finally {pendingReads--;}
    }
    return Object.freeze({
      register(owner, callbacks) {owners.set(String(owner), callbacks); publish();},
      record(_owner, _label, authoritative = null) {
        ++requestSerial;
        if (authoritative) adopt(authoritative);
        else void refresh().catch(() => {});
      },
      async request(action) {
        if (!["undo", "redo"].includes(action)) throw new Error("Unknown manual history action");
        if (applying || needsRefresh) throw new Error(needsRefresh
          ? "Refresh the saved model before changing its history."
          : "A manual-history operation is still in progress.");
        if (!state || error) await refresh();
        if (applying || needsRefresh) throw new Error("A manual-history operation is already in progress.");
        const head = state?.[action + "_head"];
        if (!state?.["can_" + action] || !head)
          throw new Error(state?.stale
            ? "The saved model or problem changed outside manual history. No history was applied."
            : `There is no currently available manual edit to ${action}.`);
        const callback = (owners.get("viewport") || owners.get("direct"))?.[action];
        if (typeof callback !== "function") throw new Error("The geometry refresh controller is unavailable.");
        const payload = {expected_history_revision: state.revision, expected_entry_id: head.entry_id};
        applying = true; ++requestSerial; publish();
        try {
          const result = await callback(payload);
          ++requestSerial;  
          if (result?.history) adopt(result.history);
          else await refresh();
          return {label: result?.label || head.label, entry_id: head.entry_id};
        } catch (failure) {
           
          if (failure.history) adopt(failure.history);
          needsRefresh = true;
          try {
            await refresh();
            const redraw = owners.get("viewport")?.refresh;
            if (typeof redraw === "function") {await redraw(); needsRefresh = false;}
          } catch (_) {}
          throw failure;
        } finally {applying = false; publish();}
      },
      refresh, adopt,
      synchronized() {needsRefresh = false; publish();},
      counts, snapshot() {return state ? structuredClone(state) : null;},
      reading() {return pendingReads > 0;},
    });
  }
  global.ImplexityCreateManualHistory = createHistory;
  if (typeof document === "undefined") return;
  const history = createHistory({
    async read() {
      const response = await fetch("/v1/implicit/interactions/history", {credentials: "same-origin", cache: "no-store"});
      const value = await response.json();
      if (!response.ok) throw new Error(value.error || "Shared manual history could not be read.");
      return value;
    },
    changed(detail) {global.dispatchEvent(new CustomEvent("implexity:manual-history-changed", {detail}));},
  });
  global.ImplexityManualHistory = history;
  let knownState = null, refreshPending = false, deferredModelRefresh = false;
   
   
  const modelLoaded = () => global.S?.model?.loaded === true;
  async function poll() {
    if (document.hidden || refreshPending || !modelLoaded() || history.reading() || history.counts().busy) return;
    const interaction = global.ImplexityInteraction;
    if (interaction?.activeGesture || interaction?.pendingGesture || interaction?.selectionOperationPending ||
        global.ImplexityPointerCoordinator?.current?.().owner) return;
    refreshPending = true;
    try {
      const value = await history.refresh();
      if (knownState !== null && knownState !== value.state_id) deferredModelRefresh = true;
      knownState = value.state_id;
      if (deferredModelRefresh && !value.active_transaction && interaction &&
          typeof interaction._refreshCommittedGeometry === "function") {
         
        await interaction._refreshCommittedGeometry();
        deferredModelRefresh = false;
      }
    } catch (_) {   }
    finally {refreshPending = false;}
  }
  global.addEventListener("focus", poll);
  global.addEventListener("implexity:model-updated", () => {if (modelLoaded()) void history.refresh().catch(() => {});});
  document.addEventListener("visibilitychange", poll);
  global.setInterval(poll, 1500);
  void poll();
})(globalThis);
