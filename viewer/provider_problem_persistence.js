// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

(() => {
  'use strict';

  const SCHEMA = 'implexity-provider-engineering-problem/1';
  const clone = value => structuredClone(value);

  function requireObject(value, label) {
    if (!value || typeof value !== 'object' || Array.isArray(value)) {
      throw new TypeError(`${label} must be an object.`);
    }
    return value;
  }

  function recordFromResponse(payload) {
    const record = payload && typeof payload === 'object' && 'result' in payload
      ? payload.result : payload;
    requireObject(record, 'Stored provider problem');
    if (record.schema !== SCHEMA || typeof record.provider !== 'string' ||
        !record.provider || !record.problem_id) {
      throw new Error('The service did not return a bound provider problem identity.');
    }
    requireObject(record.problem, 'Stored provider problem payload');
    return record;
  }

  function unpack(payload) {
    const value = payload && typeof payload === 'object' && 'result' in payload
      ? payload.result : payload;
    requireObject(value, 'Engineering problem response');
    if (value.schema === SCHEMA) {
      const record = recordFromResponse(value);
      return {
        providerId: record.provider,
        problem: clone(record.problem),
        record: clone(record),
      };
    }
    const problem = value.problem && typeof value.problem === 'object'
      ? value.problem : value;
    requireObject(problem, 'Engineering problem');
    return {providerId: 'legacy_multiphysics_implicit', problem: clone(problem), record: null};
  }

  async function captureModel() {
    const response = await fetch('/v1/agent/state', {credentials: 'same-origin'});
    const payload = await response.json();
    const state = payload?.result ?? payload;
    const model = state?.model;
    if (!response.ok || payload.ok === false ||
        !model || ['structure_id', 'content_id'].some(key => typeof model[key] !== 'string' || !model[key])) {
      throw new Error('The current model identity is unavailable. Load a model before saving physics settings.');
    }
    return {structure_id: model.structure_id, content_id: model.content_id};
  }

  async function save({providerId, problem, provenance = {}, expectedModel, signal} = {}) {
    if (typeof providerId !== 'string' || !providerId.trim() ||
        providerId !== providerId.trim()) {
      throw new TypeError('Provider ID must be canonical nonempty text.');
    }
    requireObject(problem, 'Provider problem');
    requireObject(provenance, 'Provider problem provenance');
    const envelope = {
      schema: SCHEMA,
      provider: providerId,
      problem: clone(problem),
      provenance: clone(provenance),
    };
    if (expectedModel !== undefined) envelope.expected_model = clone(expectedModel);
    const response = await fetch('/v1/agent/action', {
      method: 'POST',
      credentials: 'same-origin',
      headers: {'content-type': 'application/json'},
      body: JSON.stringify({action: 'set_engineering_problem', payload: envelope}),
      signal,
    });
    let payload = {};
    try { payload = await response.json(); } catch (_) { payload = {}; }
    if (!response.ok || payload.ok === false) {
      const error = new Error('The provider problem could not be stored.');
      error.implexityTechnical = {
        status: response.status,
        status_text: response.statusText,
        response: payload,
      };
      throw error;
    }
    const record = recordFromResponse(payload);
    if (record.provider !== providerId) {
      throw new Error('The stored provider identity differs from the requested provider.');
    }
    return clone(record);
  }

  window.ImplexityProviderProblemPersistence = Object.freeze({
    schema: SCHEMA,
    save,
    captureModel,
    unpack,
    recordFromResponse,
  });
})();
