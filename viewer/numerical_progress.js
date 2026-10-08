// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

export function formatNumericalProgress(value) {
  if (!value || value.schema !== "implexity-numerical-progress/1" ||
      value.diagnostic_only !== true || !Number.isSafeInteger(value.sequence) ||
      value.sequence < 1 || value.sequence > 8192 ||
      !Number.isFinite(value.elapsed_s) || value.elapsed_s < 0 ||
      typeof value.event !== "string" || !/^[a-z_]{1,80}$/.test(value.event) ||
      !["started", "finished", "failed", "point"].includes(value.phase)) return null;
  const labels = {
    solver_recovery: "Solver recovery",
    optimization_trial_decision: "Optimization trial decision",
    provider_operation: "Physics evaluation", history_solve: "Coupled history solve",
    history_step_solve: "History step", history_step_converged: "History step converged",
    history_step_guess_fallback: "History step restarted from previous state",
    history_adjoint: "History adjoint", history_adjoint_step: "History adjoint step",
    implicit_solve: "Implicit solve", nonlinear_residual: "Residual evaluation",
    newton_iteration: "Newton iteration", state_jacobian_callback: "State Jacobian evaluation",
    linear_sparse_factorization: "Sparse factorization", linear_dense_factorization: "Dense factorization",
    linear_factorization_ready: "Factorization ready", linear_condition_estimate: "Condition estimate",
    linear_solve: "Linear solve", linear_residual_certification: "Linear residual check",
    line_search_residual: "Line-search trial", linear_matrix_validation: "Matrix validation",
    linear_csc_conversion: "Sparse matrix conversion", exact_krylov_condition_estimate: "Krylov condition estimate",
    exact_krylov_dispatch: "Exact Krylov solve", exact_matrix_free_dispatch: "Exact matrix-free solve"
  };
  if (!(value.event in labels)) return null;
  if (value.evaluation_role !== undefined &&
      !["authoritative_candidate", "auxiliary", "unspecified"].includes(value.evaluation_role)) return null;
  const parts = [labels[value.event] + ": " + value.phase,
                 "Elapsed: " + value.elapsed_s.toFixed(1) + " s"];
  if (value.event === "solver_recovery") {
    const recovery = solverRecoveryReport(value.solver_recovery);
    if (!recovery) return null;
    parts.push(recovery.message, "One automatic retry.");
  } else if (Object.hasOwn(value, "solver_recovery")) return null;
  if (value.event === "optimization_trial_decision") {
    const trial = value.trial_decision;
    const required = ["schema", "iteration", "attempt", "decision", "current_design_state_id", "step_fraction"];
    const optional = ["candidate_design_state_id", "objective", "armijo_bound", "directional", "error_type", "error", "reason", "unavailable_fields"];
    const decisions = {
      started: "Evaluation started", accepted: "Step accepted",
      numerical_failure: "Numerical evaluation failed", armijo_rejected: "Insufficient objective decrease",
      geometry_rejected: "Geometry feasibility rejected", admission_rejected: "Physics checks failed",
      admission_rescaled: "Provider requested a smaller step", regime_rejected: "Physical regime checks failed",
      no_trial: "No valid step found"
    };
    if (!trial || typeof trial !== "object" || Array.isArray(trial) ||
        required.some(k => !Object.hasOwn(trial, k)) ||
        Object.keys(trial).some(k => !required.includes(k) && !optional.includes(k)) ||
        trial.schema !== "implexity-optimization-trial-decision/1" ||
        !Object.hasOwn(decisions, trial.decision) ||
        !Number.isSafeInteger(trial.iteration) || trial.iteration < 0 ||
        !Number.isSafeInteger(trial.attempt) || trial.attempt < 0 ||
        !Number.isFinite(trial.step_fraction) || trial.step_fraction < 0 || trial.step_fraction > 1) return null;
    for (const key of ["current_design_state_id", "candidate_design_state_id"])
      if (Object.hasOwn(trial, key) && (typeof trial[key] !== "string" || !/^design-[0-9a-f]{64}$/.test(trial[key]))) return null;
    const signed = {objective: "Trial objective", armijo_bound: "Required Armijo bound", directional: "Directional change"};
    for (const key of Object.keys(signed))
      if (Object.hasOwn(trial, key) && !Number.isFinite(trial[key])) return null;
    const textKeys = {error_type: 128, error: 1536, reason: 1536};
    for (const [key, maximum] of Object.entries(textKeys))
      if (Object.hasOwn(trial, key) && (typeof trial[key] !== "string" || trial[key].length === 0 ||
          new TextEncoder().encode(trial[key]).length > maximum || /[\x00-\x1f\x7f-\x9f]/.test(trial[key]))) return null;
    if (Object.hasOwn(trial, "unavailable_fields") && (!Array.isArray(trial.unavailable_fields) ||
        trial.unavailable_fields.length > 3 || new Set(trial.unavailable_fields).size !== trial.unavailable_fields.length ||
        trial.unavailable_fields.some(k => !Object.hasOwn(signed, k) || Object.hasOwn(trial, k)))) return null;
    parts.push("Iteration: " + trial.iteration + "; trial: " + trial.attempt);
    parts.push(decisions[trial.decision]);
    parts.push("Trial step fraction: " + trial.step_fraction.toExponential(4));
    parts.push("Current design: " + trial.current_design_state_id);
    if (trial.candidate_design_state_id) parts.push("Candidate design: " + trial.candidate_design_state_id);
    for (const [key, label] of Object.entries(signed))
      if (Object.hasOwn(trial, key)) parts.push(label + ": " + trial[key].toExponential(6));
    for (const key of trial.unavailable_fields || []) parts.push(signed[key] + ": unavailable (nonfinite telemetry)");
    if (trial.reason) parts.push("Decision reason: " + trial.reason);
    if (trial.error) parts.push("Evaluation error" + (trial.error_type ? " (" + trial.error_type + ")" : "") + ": " + trial.error);
  } else if (Object.hasOwn(value, "trial_decision")) return null;
  for (const [key, label] of [["history_step", "History step"], ["newton_iteration", "Newton iteration"]]) {
    if (Number.isSafeInteger(value[key]) && value[key] >= 0) parts.push(label + ": " + value[key]);
  }
  if (Number.isFinite(value.residual_norm) && value.residual_norm >= 0)
    parts.push("Residual norm: " + value.residual_norm.toExponential(4));
  if (Number.isFinite(value.alpha) && value.alpha > 0 && value.alpha <= 1)
    parts.push("Trial step fraction: " + value.alpha.toExponential(3));
  if (Number.isFinite(value.duration_s) && value.duration_s >= 0)
    parts.push("Operation duration: " + (value.duration_s < 10 ? value.duration_s.toFixed(3) : value.duration_s.toFixed(1)) + " s");
  const metricMap = raw => raw && typeof raw === "object" && !Array.isArray(raw) &&
    Object.keys(raw).length <= 32 && Object.keys(raw).every(k => /^[A-Za-z][A-Za-z0-9_.:-]{0,95}$/.test(k)) ? raw : null;
  const normalized = metricMap(value.normalized_residuals), passed = metricMap(value.fields_passed);
  if (normalized && Object.values(normalized).every(v => Number.isFinite(v) && v >= 0)) {
    const rows = Object.entries(normalized);
    if (rows.length) parts.push("Scaled field residuals:");
    for (const [key, norm] of rows) {
      const flag = passed && typeof passed[key] === "boolean" ? (passed[key] ? ": passed" : ": not yet within tolerance") : "";
      parts.push("  " + key.replaceAll("_", " ") + ": " + norm.toExponential(3) + flag);
    }
  }
  if (value.phase === "failed" && typeof value.failure_reason === "string" &&
      value.failure_reason.length > 0 && value.failure_reason.length <= 1536 &&
      !/[\x00-\x1f\x7f]/.test(value.failure_reason))
    parts.push("Rejected numerical state: " + value.failure_reason);
  const rejection = value.last_rejection;
  if (value.phase !== "failed" && rejection && rejection.schema === "implexity-rejected-trial/1" &&
      Number.isSafeInteger(rejection.sequence) && rejection.sequence >= 1 && rejection.sequence <= value.sequence &&
      Number.isFinite(rejection.elapsed_s) && rejection.elapsed_s >= 0 && rejection.elapsed_s <= value.elapsed_s &&
      typeof rejection.failure_reason === "string" && rejection.failure_reason.length <= 1536 &&
      !/[\x00-\x1f\x7f]/.test(rejection.failure_reason)) {
    const step = Number.isSafeInteger(rejection.history_step) ? " in history step " + rejection.history_step : "";
    parts.push("Most recent rejected trial" + step + " (not the current operation): " + rejection.failure_reason);
  }
  if (value.field_residual_norms && typeof value.field_residual_norms === "object" &&
      !Array.isArray(value.field_residual_norms)) {
    const rows = Object.entries(value.field_residual_norms);
    if (rows.length <= 32 && rows.every(([k,v]) => /^[A-Za-z][A-Za-z0-9_.:-]{0,95}$/.test(k) && Number.isFinite(v) && v >= 0))
      for (const [key, norm] of rows) parts.push(key.replaceAll("_", " ") + ": " + norm.toExponential(4));
  }
  if (value.evaluation_role === "auxiliary")
    parts.push("Auxiliary evaluation.");
  else if (value.evaluation_role === "authoritative_candidate")
    parts.push("Candidate evaluation.");

  if (value.capped === true)
    parts.push("The diagnostic record limit has been reached; the job may still be running.");
  return parts.join("\n").replaceAll(String.fromCharCode(8212),": ");
}

export function renderNumericalProgress(element, value) {
  if (!element) return;
  const text = formatNumericalProgress(value);
  element.hidden = text === null;
  if (text === null) { element.replaceChildren(); delete element.dataset.event; return; }
  element.classList.add("solver-progress");
  element.removeAttribute("role");
  element.removeAttribute("aria-live");
  element.style.whiteSpace = "normal";
  if (!element.querySelector(".solver-progress-head")) {
    const head = document.createElement("div"); head.className = "solver-progress-head";
    const title = document.createElement("strong"); title.textContent = "Solver convergence";
    const phase = document.createElement("span"); phase.className = "solver-state";
    head.append(title, phase);
    const operation = document.createElement("p"); operation.className = "solver-operation";
    operation.setAttribute("role", "status"); operation.setAttribute("aria-live", "polite");
    const metrics = document.createElement("dl"); metrics.className = "solver-metrics";
    const fields = document.createElement("div"); fields.className = "solver-field-residuals";
    const note = document.createElement("p"); note.className = "solver-progress-note";
    const details = document.createElement("details"); details.className = "solver-details";
    const summary = document.createElement("summary"); summary.textContent = "Numerical details";
    details.append(summary, document.createElement("pre"));
    element.replaceChildren(head, operation, metrics, fields, note, details);
  }
  const state = {started:"Solving", finished:"Operation finished", failed:"Review needed", point:"Solving"}[value.phase];
  element.dataset.state = value.phase;
  element.dataset.phase = value.phase;
  const update = (selector, text) => { const target = element.querySelector(selector); if (target.textContent !== text) target.textContent = text; };
  update(".solver-state", state);
  update(".solver-operation", text.split("\n")[0].split(": ")[0]);
  const metrics = [["Elapsed / s", value.elapsed_s.toFixed(1)]];
  if (Number.isFinite(value.residual_norm) && value.residual_norm >= 0) metrics.push(["Residual norm", value.residual_norm.toExponential(3)]);
  if (Number.isSafeInteger(value.newton_iteration) && value.newton_iteration >= 0) metrics.push(["Newton iteration", String(value.newton_iteration)]);
  if (Number.isSafeInteger(value.history_step) && value.history_step >= 0) metrics.push(["History step", String(value.history_step)]);
  const metricKey = JSON.stringify(metrics);
  const grid = element.querySelector(".solver-metrics");
  if (grid.dataset.key !== metricKey) {
    grid.dataset.key = metricKey;
    grid.replaceChildren(...metrics.map(([name, value]) => {
      const row = document.createElement("div"); const label = document.createElement("dt"); label.textContent = name;
      const number = document.createElement("dd"); number.textContent = value; row.append(label, number); return row;
    }));
  }
  const norms = value.normalized_residuals;
  const valid = norms && typeof norms === "object" && !Array.isArray(norms) && Object.keys(norms).length <= 32 && Object.entries(norms).every(([name, norm]) => /^[A-Za-z][A-Za-z0-9_.:-]{0,95}$/.test(name) && Number.isFinite(norm) && norm >= 0);
  const rows = valid ? Object.entries(norms) : [];
  const fields = element.querySelector(".solver-field-residuals"); fields.hidden = !rows.length;
  const fieldKey = JSON.stringify([rows, value.fields_passed]);
  if (fields.dataset.key !== fieldKey) {
    fields.dataset.key = fieldKey;
    fields.replaceChildren(...rows.map(([name, norm]) => {
      const row = document.createElement("div"); row.className = "solver-field-row";
      const label = document.createElement("span"); label.textContent = name.replaceAll("_", " ");
      const number = document.createElement("span"); number.className = "solver-field-value"; number.textContent = norm.toExponential(3);
      const status = document.createElement("span"); status.className = "solver-field-status";
      const passed = value.fields_passed && Object.hasOwn(value.fields_passed, name) ? value.fields_passed[name] : undefined;
      status.textContent = passed === true ? "Within tolerance" : passed === false ? "Outside tolerance" : "Not reported";
      if (typeof passed === "boolean") row.dataset.passed = String(passed);
      row.append(label, number, status); return row;
    }));
  }
  const note=element.querySelector(".solver-progress-note");
  note.hidden=value.capped!==true;
  update(".solver-progress-note", value.capped===true ? "Follow the run status for further progress." : "");
  update(".solver-details pre", text);
}

export function solverRecoveryReport(value) {
  const keys = ["schema", "operation", "status", "attempt_count", "retry_count", "max_retries", "settings_changed", "message", "first_failure", "final_failure"];
  const text = (value, limit) => typeof value === "string" && value.length > 0 && new TextEncoder().encode(value).length <= limit && !/[\x00-\x1f\x7f-\x9f]/.test(value);
  const detail = value => value && typeof value === "object" && !Array.isArray(value) && Object.keys(value).length === 2 && text(value.error_type,128) && text(value.reason,1536);
  if (!value || typeof value !== "object" || Array.isArray(value) || Object.keys(value).length !== keys.length || keys.some(k => !Object.hasOwn(value,k)) || value.schema !== "implexity-solver-recovery/1" || !["retrying","recovered","needs_attention"].includes(value.status) || value.attempt_count !== 2 || value.retry_count !== 1 || value.max_retries !== 1 || value.settings_changed !== false || !text(value.operation,128) || !text(value.message,512) || !detail(value.first_failure) || (value.status === "needs_attention" ? !detail(value.final_failure) : value.final_failure !== null)) return null;
  return value;
}
export function renderSolverRecovery(anchor, value) {
  if (!anchor) return;
  let panel = anchor.nextElementSibling;
  if (!panel?.classList.contains("solver-recovery-notice")) {
    panel = document.createElement("section");
    panel.className = "solver-recovery-notice";
    panel.setAttribute("role","status");
    panel.setAttribute("aria-live","polite");
    panel.setAttribute("aria-atomic","true");
    anchor.after(panel);
  }
  const report = solverRecoveryReport(value);
  const progressState = anchor.querySelector(".solver-state");
  if (progressState && anchor.dataset.phase) {
    progressState.textContent = report ? {retrying:"Retrying", recovered:"Converged on retry", needs_attention:"Needs review"}[report.status] : {started:"Solving", finished:"Operation finished", failed:"Review needed", point:"Solving"}[anchor.dataset.phase];
    anchor.dataset.state = report ? {retrying:"started", recovered:"finished", needs_attention:"failed"}[report.status] : anchor.dataset.phase;
  }
  if (!report) { panel.hidden = true; panel.replaceChildren(); delete panel.dataset.signature; return; }
  const signature = JSON.stringify(report);
  if (panel.dataset.signature === signature) return;
  panel.dataset.signature = signature;
  panel.dataset.state = report.status;
  panel.hidden = false;
  panel.replaceChildren();
  const heading = document.createElement("div"); heading.className = "solver-progress-head solver-recovery-heading";
  const title = document.createElement("strong"); title.textContent = {retrying:"Retrying the solve",recovered:"Solve converged on retry",needs_attention:"Solver needs attention"}[report.status];
  const badge = document.createElement("span"); badge.className = "solver-state"; badge.textContent = "1 automatic retry";
  heading.append(title,badge);
  const message = document.createElement("p"); message.textContent = {retrying:"Retrying once with the same settings.",recovered:"",needs_attention:"The retry was unsuccessful. Review the settings and boundary conditions."}[report.status];
  message.hidden=report.status==="recovered";
  panel.append(heading,message);
  if (report.status === "needs_attention") {
    const actions = document.createElement("div"); actions.className = "solver-recovery-actions";
    const review = document.createElement("button"); review.type = "button"; review.textContent = "Review settings";
    review.onclick = () => window.implexityWorkbench?.setStep?.("physics");
    const dismiss = document.createElement("button"); dismiss.type = "button"; dismiss.textContent = "Dismiss notice"; dismiss.onclick = () => { panel.hidden = true; };
    actions.append(review,dismiss); panel.append(actions);
  }
  const details = document.createElement("details"); details.className = "solver-details";
  const summary = document.createElement("summary"); summary.textContent = "Solver details";
  const contents = document.createElement("pre");
  contents.textContent = "Operation: " + report.operation.replaceAll("_"," ") + "\nFirst attempt: " + report.first_failure.error_type + "\n" + report.first_failure.reason + (report.final_failure ? "\n\nRetry: " + report.final_failure.error_type + "\n" + report.final_failure.reason : "") + "\n\nPhysical settings and solver tolerances were kept unchanged.";
  details.append(summary,contents); panel.append(details);
}
