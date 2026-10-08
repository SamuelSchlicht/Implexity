// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

 
 
 
 
 
 
 
 
 
 
 
 
 
 
(() => {
  "use strict";

  const $ = (id) => document.getElementById(id);
   
  const SERIES_COLOURS = ["#16776d", "#0072b2", "#e69f00", "#cc79a7", "#56b4e9", "#d55e00", "#009e73"];
   
  const PLOT = {
    background: "#ffffff", grid: "#e2e9e6", axis: "#ced9d5", text: "#586b6d", marker: "#c0392b",
    font: '11px "Segoe UI", Arial, sans-serif',
  };
  const EMBEDDED = new URLSearchParams(location.search).get("embedded") === "1";
  if (EMBEDDED) document.body.classList.add("dr-embedded");
  const state = {
    store: "",
    info: null,
    cycles: [],
    cycle: null,
    frames: [],
    pos: 0,
    timer: null,
    series: null,
    images: new Map(),
    generation: 0,
    selection: 0,
    abort: new AbortController(),
    available: true,
  };

  function status(message) {
    $("drStatus").textContent = message;
  }

  async function getJSON(url) {
    const response = await fetch(url, { cache: "no-store" });
    const body = await response.json().catch(() => null);
    if (!response.ok) {
      throw new Error(body && body.error ? body.error : `HTTP ${response.status}`);
    }
    return body;
  }

  function fill(select, items, { empty = null, keep = true } = {}) {
    const previous = select.value;
    select.textContent = "";
    if (empty !== null) {
      select.append(new Option(empty, ""));
    }
    for (const item of items) {
      const [value, label] = Array.isArray(item) ? item : [item, item];
      select.append(new Option(label, value));
    }
    if (keep && [...select.options].some((o) => o.value === previous)) {
      select.value = previous;
    }
  }

   
  function allFields() {
    const m = state.info ? state.info.manifest : null;
    if (!m) return [];
    return [...m.fields, ...(m.meshes || []).flatMap((x) => x.attributes || [])];
  }

  function fieldsOf(kinds, grids = null) {
    return allFields()
      .filter((f) => kinds.includes(f.kind) && (grids === null || grids.includes(f.grid)))
      .map((f) => [f.name, `${f.label} [${f.unit}]`]);
  }

  function storedOf(name) {
     
    const stored = name.includes(":") ? name.slice(name.indexOf(":") + 1) : name;
    return allFields().find((x) => x.name === stored) || null;
  }

  function gridOf(name) {
    const f = storedOf(name);
    return f ? state.info.manifest.grids.find((g) => g.name === f.grid) || null : null;
  }

   
  function meshOf(name) {
    const f = storedOf(name);
    if (!f) return null;
    const mesh = f.grid.endsWith("/cells") ? f.grid.slice(0, -6) : f.grid;
    return (state.info.manifest.meshes || []).find((x) => x.name === mesh) || null;
  }

  function dimsOf(name) {
    const g = gridOf(name);
    if (g) return g.shape.length;
    const m = meshOf(name);
    return m ? (m.cell === "tetrahedron" ? 3 : 2) : 0;
  }

   
  function centreOf(name) {
    const g = gridOf(name);
    if (g) return g.origin.map((o, a) => o + 0.5 * (g.shape[a] - 1) * g.spacing[a]);
    const m = meshOf(name);
    const b = m && ((state.info.renderable || {}).mesh_bounds || {})[m.name];
    return b ? b[0].map((lo, a) => 0.5 * (lo + b[1][a])) : null;
  }

   
  function bodyLocations(displacement) {
    const f = storedOf(displacement);
    if (!f) return [];
    const cells = ((state.info.renderable || {}).cell_grids || []).filter((c) => c.nodes === f.grid).map((c) => c.cells);
    return [f.grid, `${f.grid}/cells`, ...cells];
  }

  function fillBody() {
    const d = $("drDeform").value;
    const at = d ? bodyLocations(d) : null;
    fill($("drDeformMask"), fieldsOf(["occupancy", "scalar"], at), { empty: "auto (occupancy on the solid's cells)" });
    fill($("drDeformColour"), fieldsOf(["scalar", "occupancy", "displacement"], at), { empty: "uniform" });
  }

  function deformScale() {
    return Number($("drScale").value) * Number($("drScaleMul").value);
  }

  function viewParams() {
    const p = new URLSearchParams();
    const field = $("drField").value;
    p.set("field", field);
    const set = (key, value) => {
      if (value !== "" && value !== null && value !== undefined) p.set(key, value);
    };
    set("component", $("drComponent").value);
    set("mode", $("drMode").value);
    set("colormap", $("drColormap").value);
    set("range", $("drRange").value);
    set("background", $("drBackground").value);
    const iso = $("drMode").value === "iso";
    if (dimsOf(field) === 3) {
      set("plane", $("drPlane").value);
      set("position", $("drPosition").value);
      if (iso) {
         
        if ($("drIsoField").value || !$("drDeform").value) set("iso_field", $("drIsoField").value);
        const centre = $("drClip").checked ? centreOf($("drDeform").value && !gridOf(field) ? $("drDeform").value : field) : null;
        if (centre) {
          set("clip_point", centre.join(","));
          set("clip_normal", "0,-1,0");
        }
      }
    }
    if (!iso) {
      set("occupancy", $("drOccupancy").value);
      if ($("drOccupancy").value && $("drFill").checked) set("fill_occupancy", "1");
      if ($("drFlowStyle").value && $("drFlowField").value) {
        set("flow_style", $("drFlowStyle").value);
        set("flow_field", $("drFlowField").value);
      }
    }
    if ($("drDeform").value) {
      set("deform_field", $("drDeform").value);
      set("deform_scale", String(deformScale()));
      set("deform_mask", $("drDeformMask").value);
      set("deform_colour", $("drDeformColour").value);
      set("deform_mask_threshold", URL_VIEW.get("deform_mask_threshold"));
    }
    const width = Math.max(320, Math.min(1200, document.querySelector(".dr-image").clientWidth - 16));
    p.set("width_px", String(Math.round(width)));
    p.set("height_px", String(Math.max(200, Math.min(900, Math.round(width * 0.6)))));
    return p;
  }

  function frameURL(index) {
    const p = viewParams();
    p.set("id", state.store);
    p.set("index", String(index));
    return `/v1/dynamic/frame?${p}`;
  }

  async function frameImage(index, generation) {
    const url = frameURL(index);
    if (state.images.has(url)) return state.images.get(url);
    const response = await fetch(url, { cache: "no-store", signal: state.abort.signal });
    if (!response.ok) {
      const body = await response.json().catch(() => null);
      throw new Error(body && body.error ? body.error : `HTTP ${response.status}`);
    }
    const blob = await response.blob();
    if (generation !== state.generation) return null;
    const objectURL = URL.createObjectURL(blob);
    state.images.set(url, objectURL);
    return objectURL;
  }

  function clearImages() {
     
    state.abort.abort();
    state.abort = new AbortController();
    for (const url of state.images.values()) URL.revokeObjectURL(url);
    state.images.clear();
    state.generation += 1;
  }

  function currentFrame() {
    return state.frames[state.pos] || null;
  }

  function info() {
    const f = currentFrame();
    const unit = state.info ? state.info.manifest.time.unit : "";
    $("drInfo").textContent = f
      ? `t = ${f.t.toPrecision(6)} ${unit}` + (f.phase !== null ? `   phase = ${f.phase.toFixed(3)}` : "") + (f.cycle !== null ? `   cycle ${f.cycle}` : "")
      : "";
  }

  async function show() {
    if (!state.available) return;
    const f = currentFrame();
    info();
    drawPlot();
    if (!f) return;
    const generation = state.generation;
    try {
      const url = await frameImage(f.index, generation);
      if (url && generation === state.generation && currentFrame() === f) {
        $("drImage").src = url;
        status("");
      }
    } catch (e) {
      if (e.name === "AbortError" || generation !== state.generation || !state.available) return;
      stop();
      status(`render refused: ${e.message}`);
    }
  }

  async function prefetch() {
    const generation = state.generation;
    for (const f of state.frames) {
      if (generation !== state.generation || !state.available) return;
      try {
        await frameImage(f.index, generation);
      } catch (e) {
        return;
      }
    }
  }

  function selectCycle() {
    if (!state.info) return;
    const value = $("drCycle").value;
    const all = state.info.frames_index.map(([seq, t, phase, cycle], i) => ({
      seq, t, phase, cycle, index: state.info.frames_index_offset + i,
    }));
    if (value === "all" || !state.cycles.length) {
      state.frames = all;
    } else {
      const c = Number(value);
      state.frames = all.filter((f) => f.cycle === c).sort((a, b) => a.phase - b.phase);
    }
    state.pos = Math.min(state.pos, Math.max(0, state.frames.length - 1));
    $("drScrub").max = String(Math.max(0, state.frames.length - 1));
    $("drScrub").value = String(state.pos);
  }

  function renderView() {
    if (!state.available || !state.info) return;
    clearImages();
    const phaseAverage = $("drMode").value === "phase_average";
    $("drPlay").disabled = phaseAverage;
    $("drScrub").disabled = phaseAverage;
    updateExport();
    if (phaseAverage) {
      stop();
      const p = viewParams();
      p.set("ids", state.store);
      p.set("phase_bins", "8");
      p.set("width_px", "360");
      p.set("height_px", "220");
      loadSheet(`/v1/dynamic/sheet?${p}`, $("drImage"));
      return;
    }
    show().then(prefetch);
  }

  async function loadSheet(url, img) {
    const generation = state.generation;
    try {
      const response = await fetch(url, { cache: "no-store" });
      if (!response.ok) {
        const body = await response.json().catch(() => null);
        throw new Error(body && body.error ? body.error : `HTTP ${response.status}`);
      }
      const blob = await response.blob();
      if (!state.available || generation !== state.generation) return;
      img.src = URL.createObjectURL(blob);
      status("");
    } catch (e) {
      if (state.available && generation === state.generation) status(`render refused: ${e.message}`);
    }
  }

  function updateExport() {
    if (!state.available || !state.info) return;
    const p = viewParams();
     
    const w = Math.min(720, Number(p.get("width_px")));
    p.set("width_px", String(w));
    p.set("height_px", String(Math.max(200, Math.round(w * 0.6))));
    p.set("id", state.store);
    p.set("format", $("drFormat").value);
    p.set("frames_per_cycle", String(Math.max(2, Math.min(96, state.frames.length || 24))));
    p.set("fps", $("drFps").value || "8");
    if ($("drCycle").value && $("drCycle").value !== "all") p.set("cycle", $("drCycle").value);
    if (p.get("mode") === "phase_average") p.set("phase_bins", "12");
    $("drExport").href = `/v1/dynamic/animation?${p}`;
  }

  function stop() {
    if (state.timer) clearInterval(state.timer);
    state.timer = null;
    $("drPlay").textContent = "Play";
    $("drPlay").setAttribute("aria-pressed", "false");
  }

  function play() {
    if (state.timer) {
      stop();
      return;
    }
    const fps = Math.max(1, Math.min(30, Number($("drFps").value) || 8));
    $("drPlay").textContent = "Pause";
    $("drPlay").setAttribute("aria-pressed", "true");
    state.timer = setInterval(() => {
      if (!state.frames.length) return;
      state.pos = (state.pos + 1) % state.frames.length;
      $("drScrub").value = String(state.pos);
      show();
    }, 1000 / fps);
  }

   

  function plotData() {
    const s = state.series;
    if (!s) return [];
    const chosen = [...$("drSeries").selectedOptions].map((o) => o.value);
    const names = chosen.length ? chosen : Object.keys(s.series).slice(0, 3);
    return names.filter((n) => s.series[n]).map((n) => [n, s.series[n]]);
  }

  function axes(ctx, w, h, xr, yr, xlabel, ylabel, log) {
    ctx.fillStyle = PLOT.background;
    ctx.fillRect(0, 0, w, h);
    ctx.strokeStyle = PLOT.grid;
    ctx.fillStyle = PLOT.text;
    ctx.font = PLOT.font;
    ctx.lineWidth = 1;
    const L = 58, R = 12, T = 12, B = 30;
    const px = (x) => L + ((x - xr[0]) / (xr[1] - xr[0] || 1)) * (w - L - R);
    const py = (y) => h - B - (((log ? Math.log10(y) : y) - yr[0]) / (yr[1] - yr[0] || 1)) * (h - T - B);
    for (let k = 0; k <= 4; k++) {
      const x = xr[0] + (k / 4) * (xr[1] - xr[0]);
      const y = yr[0] + (k / 4) * (yr[1] - yr[0]);
      ctx.beginPath();
      ctx.moveTo(px(x), T);
      ctx.lineTo(px(x), h - B);
      ctx.moveTo(L, h - B - (k / 4) * (h - T - B));
      ctx.lineTo(w - R, h - B - (k / 4) * (h - T - B));
      ctx.stroke();
      ctx.fillText(Number(x.toPrecision(3)).toString(), px(x) - 12, h - B + 14);
      ctx.fillText(log ? `1e${y.toFixed(1)}` : Number(y.toPrecision(3)).toString(), 4, h - B - (k / 4) * (h - T - B) + 4);
    }
    ctx.strokeStyle = PLOT.axis;
    ctx.beginPath();
    ctx.moveTo(L, T);
    ctx.lineTo(L, h - B);
    ctx.lineTo(w - R, h - B);
    ctx.stroke();
    ctx.fillText(xlabel, w / 2 - 30, h - 4);
    ctx.fillText(ylabel, L + 4, T + 10);
    ctx.lineWidth = 1.5;
    return { px, py };
  }

  function extent(values, log) {
    let lo = Infinity, hi = -Infinity;
    for (const v of values) {
      if (!Number.isFinite(v) || (log && v <= 0)) continue;
      const x = log ? Math.log10(v) : v;
      lo = Math.min(lo, x);
      hi = Math.max(hi, x);
    }
    if (!(lo <= hi)) return [0, 1];
    if (hi - lo < 1e-300) return [lo - 1, hi + 1];
    const m = 0.05 * (hi - lo);
    return [lo - m, hi + m];
  }

  function drawPlot() {
    const canvas = $("drPlot");
    const ctx = canvas.getContext("2d");
    const w = canvas.width = Math.max(320, canvas.clientWidth || 640);
    const h = canvas.height = 300;
    const data = plotData();
    const kind = $("drPlotKind").value;
    const unit = state.info ? state.info.manifest.time.unit : "";
    if (!data.length) {
      ctx.fillStyle = PLOT.background;
      ctx.fillRect(0, 0, w, h);
      ctx.fillStyle = PLOT.text;
      ctx.font = PLOT.font;
      ctx.fillText("no series in this store", 20, 30);
      return;
    }
    const f = currentFrame();
    if (kind === "spectrum") {
      const spectra = data.map(([n]) => [n, state.series.spectrum && state.series.spectrum[n]]).filter(([, s]) => s);
      const xs = spectra.flatMap(([, s]) => s.frequency.slice(1));
      const ys = spectra.flatMap(([, s]) => s.amplitude.slice(1));
      const { px, py } = axes(ctx, w, h, extent(xs, false), extent(ys, true), `frequency [1/${unit}]`, "amplitude (log)", true);
      spectra.forEach(([n, s], k) => {
        ctx.strokeStyle = SERIES_COLOURS[k % SERIES_COLOURS.length];
        ctx.beginPath();
        let started = false;
        for (let i = 1; i < s.frequency.length; i++) {
          if (!(s.amplitude[i] > 0)) continue;
          const [x, y] = [px(s.frequency[i]), py(s.amplitude[i])];
          if (started) ctx.lineTo(x, y); else ctx.moveTo(x, y);
          started = true;
        }
        ctx.stroke();
        ctx.fillStyle = SERIES_COLOURS[k % SERIES_COLOURS.length];
        ctx.fillText(`${n}: f0 = ${s.dominant_frequency ? s.dominant_frequency.toPrecision(5) : "-"}`, w - 200, 20 + 14 * k);
      });
      return;
    }
    if (kind === "portrait") {
      const [n, s] = data[0];
      const dx = s.values.map((v, i) => {
        const a = Math.max(0, i - 1), b = Math.min(s.values.length - 1, i + 1);
        return s.t[b] > s.t[a] ? (s.values[b] - s.values[a]) / (s.t[b] - s.t[a]) : NaN;
      });
      const { px, py } = axes(ctx, w, h, extent(s.values, false), extent(dx, false), n, `d${n}/dt`, false);
      ctx.strokeStyle = SERIES_COLOURS[0];
      ctx.beginPath();
      s.values.forEach((v, i) => (i ? ctx.lineTo(px(v), py(dx[i])) : ctx.moveTo(px(v), py(dx[i]))));
      ctx.stroke();
      if (f) {
        const i = nearestIndex(s.t, f.t);
        ctx.fillStyle = PLOT.marker;
        ctx.beginPath();
        ctx.arc(px(s.values[i]), py(dx[i]), 4, 0, 2 * Math.PI);
        ctx.fill();
      }
      return;
    }
    const ts = data.flatMap(([, s]) => s.t);
    const ys = data.flatMap(([, s]) => s.values);
    const units = new Set(data.map(([, s]) => s.unit));
    const { px, py } = axes(ctx, w, h, [Math.min(...ts), Math.max(...ts)], extent(ys, false), `t [${unit}]`, [...units].join(", "), false);
    data.forEach(([n, s], k) => {
      ctx.strokeStyle = SERIES_COLOURS[k % SERIES_COLOURS.length];
      ctx.beginPath();
      s.t.forEach((t, i) => (i ? ctx.lineTo(px(t), py(s.values[i])) : ctx.moveTo(px(t), py(s.values[i]))));
      ctx.stroke();
      ctx.fillStyle = SERIES_COLOURS[k % SERIES_COLOURS.length];
      ctx.fillText(`${n} (${s.role || "series"})`, w - 180, 20 + 14 * k);
    });
    if (f) {
      ctx.strokeStyle = PLOT.marker;
      ctx.beginPath();
      ctx.moveTo(px(f.t), 12);
      ctx.lineTo(px(f.t), h - 30);
      ctx.stroke();
    }
  }

  function nearestIndex(ts, t) {
    let best = 0;
    for (let i = 1; i < ts.length; i++) if (Math.abs(ts[i] - t) < Math.abs(ts[best] - t)) best = i;
    return best;
  }

  function stats() {
    const s = state.series;
    if (!s) {
      $("drStats").textContent = "";
      return;
    }
    const lines = plotData().map(([n, x]) => {
      const st = x.statistics || {};
      const sp = s.spectrum && s.spectrum[n];
      const f0 = sp && sp.dominant_frequency ? `  f0 ${sp.dominant_frequency.toPrecision(5)}` : "";
      return `${n}: mean ${Number(st.mean).toPrecision(4)}  rms ${Number(st.rms).toPrecision(4)}  min ${Number(st.min).toPrecision(4)}  max ${Number(st.max).toPrecision(4)}${f0}`;
    });
    $("drStats").textContent = lines.join("\n");
  }

  function seekTime(t) {
    let best = 0;
    state.frames.forEach((f, i) => {
      if (Math.abs(f.t - t) < Math.abs(state.frames[best].t - t)) best = i;
    });
    state.pos = best;
    $("drScrub").value = String(best);
    show();
  }

   

  function configureControls() {
    const m = state.info.manifest;
    const derived = ((state.info.renderable || {}).derived || []).map((d) => [d, `${d.replace(":", " of ")} (derived)`]);
    const all = [...m.fields.map((f) => [f.name, `${f.label} [${f.unit}] (${f.kind})`]), ...derived];
    fill($("drField"), all);
    const threeD = allFields().some((f) => dimsOf(f.name) === 3);
    $("drSliceBox").hidden = !threeD;
    const gridNames = m.grids.map((g) => g.name);
    fill($("drIsoField"), fieldsOf(["occupancy", "scalar"], gridNames), { empty: "first occupancy / solid only" });
    fill($("drOccupancy"), fieldsOf(["occupancy"], gridNames), { empty: "none" });
    fill($("drFlowField"), fieldsOf(["vector"], gridNames), { empty: "none" });
    fill($("drDeform"), fieldsOf(["displacement"]), { empty: "none" });
    fillBody();
     
     
    if ($("drMode").value === "iso" && !$("drIsoField").value && !$("drDeform").value) {
      const g = gridOf($("drField").value);
      if (!(g && fieldsOf(["occupancy"], [g.name]).length)) {
        const d = fieldsOf(["displacement"]);
        if (d.length && dimsOf(d[0][0]) === 3) {
          $("drDeform").value = d[0][0];
          fillBody();
        } else {
          $("drMode").value = "section";
        }
      }
    }
     
    const own = gridOf($("drField").value);
    const same = own ? fieldsOf(["occupancy"], [own.name]) : [];
    if (!$("drOccupancy").value && same.length) $("drOccupancy").value = same[0][0];
    state.cycles = [...new Set(state.info.frames_index.map((r) => r[3]).filter((c) => c !== null))];
    const counts = state.cycles.map((c) => state.info.frames_index.filter((r) => r[3] === c).length);
    const most = Math.max(0, ...counts);
    const last = state.cycles.filter((c, i) => counts[i] === most).pop();
    fill($("drCycle"), [["all", "all frames"], ...state.cycles.map((c) => [String(c), `cycle ${c}`])], { keep: false });
    $("drCycle").value = last !== undefined ? String(last) : "all";
    fill($("drSeries"), Object.keys((state.series && state.series.series) || {}));
    for (const o of $("drSeries").options) o.selected = o.index < 3;
    fill($("drCompare"), [...$("drStore").options].map((o) => o.value).filter((v) => v && v !== state.store));
  }

   
   
   
   
   
  const URL_VIEW = new URLSearchParams(location.search);
  const URL_KEYS = {
    field: "drField", component: "drComponent", mode: "drMode", colormap: "drColormap", range: "drRange",
    background: "drBackground", plane: "drPlane", position: "drPosition", occupancy: "drOccupancy",
    flow_style: "drFlowStyle", flow_field: "drFlowField", deform_field: "drDeform",
    deform_mask: "drDeformMask", deform_colour: "drDeformColour", iso_field: "drIsoField", cycle: "drCycle",
  };

  function applyURLView() {
    for (const [key, id] of Object.entries(URL_KEYS)) {
      const value = URL_VIEW.get(key);
      const el = $(id);
      if (value === null || !el) continue;
      if (el.tagName === "SELECT" && ![...el.options].some((o) => o.value === value)) continue;
      el.value = value;
    }
    if (URL_VIEW.get("fill_occupancy") === "1") $("drFill").checked = true;
    if (URL_VIEW.get("clip") === "1") $("drClip").checked = true;
    const scale = Number(URL_VIEW.get("deform_scale"));
    if (scale > 0) {
       
      const mul = [1, 10, 100, 1000, 10000].find((k) => scale / k <= 20) || 10000;
      $("drScaleMul").value = String(mul);
      $("drScale").value = String(Math.min(20, scale / mul));
    }
    fillBody();
    for (const key of ["deform_mask", "deform_colour"]) {
      const value = URL_VIEW.get(key);
      if (value !== null && [...$(URL_KEYS[key]).options].some((o) => o.value === value)) $(URL_KEYS[key]).value = value;
    }
    $("drScaleOut").textContent = String(deformScale());
  }

  function invalidateSelection() {
    const selection = ++state.selection;
    stop();
    clearImages();
    state.loaded = "";
    state.info = null;
    state.series = null;
    state.cycles = [];
    state.frames = [];
    state.pos = 0;
    $("drImage").removeAttribute("src");
    $("drCompareImage").removeAttribute("src");
    $("drCompareBox").hidden = true;
    $("drInfo").textContent = "";
    $("drStats").textContent = "";
    $("drExport").removeAttribute("href");
    drawPlot();
    return selection;
  }

  async function selectStore(id) {
    const selection = invalidateSelection();
    state.store = id;
    const current = () => selection === state.selection && state.available && state.store === id;
    if (!id || !state.available) return;
    status(`Loading ${id}…`);
    try {
      const info = await getJSON(`/v1/dynamic/store?id=${encodeURIComponent(id)}`);
      if (!current()) return;
      const series = info.series_samples > 0
        ? await getJSON(`/v1/dynamic/series?id=${encodeURIComponent(id)}&max_points=1500&spectrum=1`)
        : null;
      if (!current()) return;
      state.info = info;
      state.series = series;
      configureControls();
      if (URL_VIEW.get("store") === id) applyURLView();
      selectCycle();
      if (URL_VIEW.get("store") === id && URL_VIEW.get("index") !== null) {
        const index = Number(URL_VIEW.get("index"));
        const at = state.frames.findIndex((f) => f.index === index);
        if (at >= 0) {
          state.pos = at;
          $("drScrub").value = String(at);
        }
      }
      stats();
      status(`${state.info.frames} frames, ${state.cycles.length} cycles retained, period ${state.info.period ?? "none"}`);
      renderView();
      state.loaded = id;
    } catch (e) {
      if (current()) status(`could not open ${id}: ${e.message}`);
    }
  }

  async function loadStores(preselect) {
    if (!state.available) return;
    const selection = invalidateSelection();
    const current = () => selection === state.selection && state.available;
    try {
      const list = await getJSON("/v1/dynamic/stores?limit=200");
      if (!current()) return;
      const rows = list.stores.map((s) => [s.store, `${s.store} (${s.frames} frames${s.provenance && s.provenance.design_state_id ? ", design " + String(s.provenance.design_state_id).slice(0, 10) : ""})`]);
      fill($("drStore"), rows, { empty: rows.length ? null : "no stores" });
      if (!rows.length) {
        status("No dynamic results are captured yet: dynamic providers write stores during evaluations and optimisation jobs.");
        return;
      }
      const target = preselect && rows.some((r) => r[0] === preselect) ? preselect : rows[rows.length - 1][0];
      $("drStore").value = target;
      await selectStore(target);
    } catch (e) {
      if (current()) status(`dynamic results unavailable: ${e.message}`);
    }
  }

   

   
   
  async function extensionServed() {
    const response = await fetch("/v1/agent/capabilities", { cache: "no-store", credentials: "same-origin" });
    if (!response.ok) return null;
    const body = await response.json().catch(() => null);
    const ext = body && body.rust_extension;
    const actions = ext && Array.isArray(ext.actions) ? ext.actions : [];
    return actions.some((a) => a && a.name === "inspect_dynamic_results");
  }

   
   
  function setAvailable(on) {
    if (on === state.available) return;
    state.available = on;
    if (!on) invalidateSelection();
    document.querySelector(".dr-main").hidden = !on;
    $("drUnavailable").hidden = on;
    for (const id of ["drStore", "drRefresh"]) $(id).disabled = !on;
    status(on ? "" : "Dynamic results are not available now.");
    if (on) loadStores(state.store || URL_VIEW.get("store"));
  }

  async function watchAvailability() {
    try {
      const served = await extensionServed();
      if (served !== null) setAvailable(served);
    } catch (e) {
       
    }
  }

   

  function wire() {
    $("drStore").addEventListener("change", () => selectStore($("drStore").value));
    $("drRefresh").addEventListener("click", () => loadStores(state.store));
    for (const id of ["drField", "drComponent", "drMode", "drColormap", "drRange", "drBackground", "drPlane", "drPosition",
      "drIsoField", "drClip", "drOccupancy", "drFill", "drFlowStyle", "drFlowField", "drDeform", "drDeformMask",
      "drDeformColour", "drScale", "drScaleMul"]) {
      $(id).addEventListener("change", renderView);
    }
     
    $("drDeform").addEventListener("change", fillBody, { capture: true });
    for (const id of ["drScale", "drScaleMul"]) {
      $(id).addEventListener("input", () => { $("drScaleOut").textContent = String(deformScale()); });
    }
    $("drCycle").addEventListener("change", () => { selectCycle(); renderView(); });
    $("drScrub").addEventListener("input", () => { state.pos = Number($("drScrub").value); show(); });
    $("drPlay").addEventListener("click", play);
    $("drFps").addEventListener("change", () => { updateExport(); if (state.timer) { stop(); play(); } });
    $("drFormat").addEventListener("change", updateExport);
    $("drSeries").addEventListener("change", () => { drawPlot(); stats(); });
    $("drPlotKind").addEventListener("change", drawPlot);
    $("drPlot").addEventListener("click", (event) => {
      if ($("drPlotKind").value !== "time" || !state.series) return;
      const rect = $("drPlot").getBoundingClientRect();
      const data = plotData();
      if (!data.length) return;
      const ts = data.flatMap(([, s]) => s.t);
      const [lo, hi] = [Math.min(...ts), Math.max(...ts)];
      const x = (event.clientX - rect.left) * ($("drPlot").width / rect.width);
      seekTime(lo + ((x - 58) / ($("drPlot").width - 70)) * (hi - lo));
    });
    $("drCompareGo").addEventListener("click", () => {
       
      state.abort.abort();
      state.abort = new AbortController();
      const others = [...$("drCompare").selectedOptions].map((o) => o.value).slice(0, 5);
      if (!others.length) {
        status("select stores to compare");
        return;
      }
      const p = viewParams();
      p.set("ids", [state.store, ...others].join(","));
      const f = currentFrame();
      p.set("phases", String(f && f.phase !== null ? f.phase : 0));
      p.set("width_px", "360");
      p.set("height_px", "240");
      $("drCompareBox").hidden = false;
      loadSheet(`/v1/dynamic/sheet?${p}`, $("drCompareImage"));
    });
    $("drCompareClose").addEventListener("click", () => { $("drCompareBox").hidden = true; });
    document.addEventListener("keydown", (event) => {
      if (event.target instanceof HTMLInputElement || event.target instanceof HTMLSelectElement) return;
      if (event.key === " ") { event.preventDefault(); play(); }
      if (event.key === "ArrowRight" && state.frames.length) { state.pos = (state.pos + 1) % state.frames.length; $("drScrub").value = String(state.pos); show(); }
      if (event.key === "ArrowLeft" && state.frames.length) { state.pos = (state.pos + state.frames.length - 1) % state.frames.length; $("drScrub").value = String(state.pos); show(); }
    });
  }

  wire();
  loadStores(new URLSearchParams(location.search).get("store"));
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") watchAvailability();
  });
  setInterval(() => {
    if (document.visibilityState === "visible") watchAvailability();
  }, 20000);
  globalThis.ImplexityDynamicResults = { state, selectStore, play, stop, seekTime, setAvailable, watchAvailability };
})();
