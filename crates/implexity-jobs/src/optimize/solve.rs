// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use implexity_authoring::physics_binding::Warm;
use implexity_core::pyobj::{py_str, repr};
use implexity_geometry::pyfmt::{fmt_f, fmt_g, g};
use implexity_io::npy::{NpyArray, NpyData};
use implexity_optim::design::NamedArrays;
use implexity_optim::numeric::float_value;
use implexity_optim::optjob::{Adam, AdamState, CheckpointInput};
use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value, json};

use super::mma_driver::{Measurement, MmaDriver, Snapshot, round3};
use super::problem::{Design, Problem};
use super::spec::{
    Classification, Drive, Free, OptimizeSpec, classify_model_steer, drive_value, json_array, model_drive_of,
    safe, select_optimizer,
};
use crate::error::{JobError, JobResult};

pub type Emit<'a> = &'a dyn Fn(&str, &Value);
pub type LogFn<'a> = &'a (dyn Fn(&str) + Send + Sync);

pub const HALTED: &str = "Halted";

fn cae(e: impl std::fmt::Display) -> JobError {
    JobError::runtime(e.to_string())
}

fn write_json_indent1(path: &Path, value: &Value) -> JobResult<()> {
    let text = implexity_core::json::dumps(value, &implexity_core::json::DumpOptions::indented(1));
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[must_use]
pub fn is_numerical_failure(e: &JobError) -> bool {
    e.is_value_error()
        || matches!(
            e.python_class(),
            "RuntimeError"
                | "ArithmeticError"
                | "FloatingPointError"
                | "ZeroDivisionError"
                | "OverflowError"
                | "NotImplementedError"
                | "RecursionError"
                | "CAEConvergenceError"
                | "CAENumericalError"
        )
}

fn failure_text(e: &JobError) -> String {
    let m = e.message();
    format!("{}: {}", e.python_class(), m.lines().next().unwrap_or(""))
}


pub fn write_design(
    out_dir: &Path,
    name: &str,
    spec: &OptimizeSpec,
    p: &NamedArrays,
    row: Option<i64>,
) -> JobResult<(f64, u64)> {
    let mut payload: Vec<(String, NpyArray)> = Vec::new();
    for fr in &spec.free {
        let a = p.get(&fr.slot).cloned().unwrap_or_else(|| ArrayD::zeros(IxDyn(&fr.shape)));
        payload.push((format!("p_{}", fr.slot), NpyArray::from_f64(&a)));
    }
    payload
        .push(("refs".into(), NpyArray::strings(&spec.free.iter().map(Free::ref_str).collect::<Vec<_>>())));
    payload.push((
        "slots".into(),
        NpyArray::strings(&spec.free.iter().map(|f| f.slot.clone()).collect::<Vec<_>>()),
    ));
    payload.push((
        "units".into(),
        NpyArray::strings(&spec.free.iter().map(|f| f.units.clone()).collect::<Vec<_>>()),
    ));
    payload.push(("solve_id".into(), NpyArray::scalar_str(&spec.digest())));
    if let Some(r) = row {
        payload.push(("row_i".into(), NpyArray::scalar_i64(r)));
    }
    implexity_optim::optjob::atomic_savez(&out_dir.join(name), false, &payload).map_err(cae)
}


pub fn read_design(path: &Path) -> JobResult<(Option<i64>, Vec<(String, ArrayD<f64>)>)> {
    let npz = implexity_io::npz::load_file(path).map_err(cae)?;
    let strings = |k: &str| match npz.get(k).map(|a| &a.data) {
        Some(NpyData::Unicode { values, .. }) => Ok(values.clone()),
        _ => Err(JobError::of("KeyError", format!("'{k} is not a file in the archive'"))),
    };
    let refs = strings("refs")?;
    let slots = strings("slots")?;
    let marker = npz.get("row_i").and_then(NpyArray::to_i64).and_then(|a| a.iter().next().copied());
    let mut out = Vec::new();
    for (r, s) in refs.iter().zip(&slots) {
        let a = npz
            .get(&format!("p_{s}"))
            .and_then(NpyArray::to_f64)
            .ok_or_else(|| JobError::of("KeyError", format!("'p_{s} is not a file in the archive'")))?;
        out.push((r.clone(), a));
    }
    Ok((marker, out))
}


pub fn load_model_npz(path: &Path) -> JobResult<Vec<(String, ArrayD<f64>)>> {
    Ok(read_design(path)?.1)
}

#[derive(Clone, Debug, Default)]
struct Best {
    fields: Map<String, Value>,
    p: Option<NamedArrays>,
}

impl Best {
    fn empty(regime_key: Value, first_l: Value) -> Self {
        let mut fields = Map::new();
        fields.insert("L".into(), float_value(f64::INFINITY));
        if !regime_key.is_null() || !first_l.is_null() {
            fields.insert("regime_key".into(), regime_key);
            fields.insert("first_L".into(), first_l);
        }
        Self { fields, p: None }
    }

    fn l(&self) -> f64 {
        self.fields.get("L").and_then(Value::as_f64).unwrap_or(f64::INFINITY)
    }

    fn get(&self, k: &str) -> Value {
        self.fields.get(k).cloned().unwrap_or(Value::Null)
    }
}

fn row_l(r: &Value) -> f64 {
    r.get("L").and_then(Value::as_f64).unwrap_or(f64::NAN)
}

fn best_of_rows(rows: &[Value], driver_kind: &str) -> Best {
    let Some(last) = rows.last() else { return Best::empty(Value::Null, Value::Null) };
    let key = last.get("regime").cloned().unwrap_or(json!(0));
    let same: Vec<&Value> = rows
        .iter()
        .filter(|r| implexity_core::pyobj::py_eq(r.get("regime").unwrap_or(&json!(0)), &key))
        .collect();
    let first = same.first().and_then(|r| r.get("L")).cloned().unwrap_or(Value::Null);
    let mut best = Best::empty(key.clone(), first.clone());

    best.fields.insert("regime_key".into(), key.clone());
    best.fields.insert("first_L".into(), first.clone());
    for r in same {
        let ok = driver_kind != "mma" || r.get("feasible").is_some_and(implexity_core::pyobj::truthy);
        if ok && row_l(r) < best.l() {
            let mut f = r.as_object().cloned().unwrap_or_default();
            f.insert("regime_key".into(), key.clone());
            f.insert("first_L".into(), first.clone());
            best = Best { fields: f, p: None };
        }
    }
    best
}

fn amin(a: &ArrayD<f64>) -> f64 {
    a.iter().copied().fold(f64::INFINITY, |x, y| if y.is_nan() || x.is_nan() { f64::NAN } else { x.min(y) })
}

fn amax(a: &ArrayD<f64>) -> f64 {
    a.iter()
        .copied()
        .fold(f64::NEG_INFINITY, |x, y| if y.is_nan() || x.is_nan() { f64::NAN } else { x.max(y) })
}

fn abs_max(a: &ArrayD<f64>) -> f64 {
    amax(&a.mapv(f64::abs))
}

fn abs_mean(a: &ArrayD<f64>) -> f64 {
    implexity_optim::numeric::array_mean(&a.mapv(f64::abs))
}

fn scalar_or_null(a: &ArrayD<f64>) -> Value {
    if a.ndim() == 0 { a.iter().next().map_or(Value::Null, |v| float_value(*v)) } else { Value::Null }
}

fn all_finite_value(v: &Value) -> bool {
    match v {
        Value::Number(n) => n.as_f64().is_some_and(f64::is_finite),
        Value::Array(a) => a.iter().all(all_finite_value),
        Value::Object(m) => m.values().all(all_finite_value),
        _ => true,
    }
}

#[derive(Clone, Debug)]
pub struct SolveOutcome {
    pub summary: Map<String, Value>,
    pub best: NamedArrays,
}

struct Loop<'a> {
    spec: Arc<OptimizeSpec>,
    out_dir: &'a Path,
    log: LogFn<'a>,
    t_run0: f64,
    n_iter: i64,
    steerable: bool,
    driver_kind: String,
    hist: Vec<Value>,
    best: Best,
    push_stats: Vec<Value>,
    timeline: Vec<Value>,
    retreats: Vec<Value>,
    regime: i64,
    lr_scale: f64,
    drive: Drive,
    steer_queue: Vec<Value>,
}

impl Loop<'_> {
    fn pending(&mut self, i: i64) -> Option<Map<String, Value>> {
        if let Some(first) = self.steer_queue.first()
            && first.get("at_iteration").and_then(Value::as_i64).unwrap_or(-1) <= i
        {
            let e = self.steer_queue.remove(0);
            let mut r = e.get("request").and_then(Value::as_object).cloned().unwrap_or_default();
            r.insert("_replayed".into(), Value::Bool(true));
            return Some(r);
        }
        let pth = self.out_dir.join("steer.json");
        if !pth.is_file() {
            return None;
        }
        let seen = self.out_dir.join(format!("steer_{:03}.req.json", self.timeline.len()));
        let read = (|| -> Result<Value, String> {
            std::fs::rename(&pth, &seen).map_err(|e| e.to_string())?;
            let text = std::fs::read_to_string(&seen).map_err(|e| e.to_string())?;
            implexity_core::json::parse_with(
                &text,
                implexity_core::json::ParseOptions { reject_duplicate_keys: false },
            )
            .map_err(|e| e.to_string())
        })();
        match read {
            Ok(Value::Object(m)) => Some(m),
            Ok(other) => {
                (self.log)(&format!("steer request unreadable, ignored: {}", py_str(&other)));
                None
            }
            Err(e) => {
                (self.log)(&format!("steer request unreadable, ignored: {e}"));
                None
            }
        }
    }

    fn write_history(&self, status: &str, prob: &Problem, mma: Option<&MmaDriver>) -> JobResult<()> {
        let hist_len = self.hist.len();
        let mut m = Map::new();
        m.insert("status".into(), json!(status));
        m.insert("kind".into(), json!("implicit_optimize"));
        m.insert("physics".into(), json!(self.spec.physics.name()));
        m.insert("case".into(), self.spec.bbox.get("name").cloned().unwrap_or(Value::Null));
        m.insert("grid".into(), self.spec.bbox.get("grid").cloned().unwrap_or(Value::Null));
        m.insert("h_mm".into(), self.spec.bbox.get("h_mm").cloned().unwrap_or(Value::Null));
        m.insert("iterations".into(), json!(hist_len));
        m.insert("total_iters".into(), json!(self.n_iter));
        m.insert(
            "L_first".into(),
            self.hist.first().and_then(|r| r.get("L")).cloned().unwrap_or(Value::Null),
        );
        m.insert("L_best".into(), if hist_len > 0 { self.best.get("L") } else { Value::Null });
        m.insert("L_last".into(), self.hist.last().and_then(|r| r.get("L")).cloned().unwrap_or(Value::Null));
        m.insert("spec".into(), self.spec.describe());
        m.insert("lattice".into(), prob.lattice());
        m.insert("probe".into(), Value::Object(prob.probe.clone()));
        m.insert("l0".into(), prob.l0.map_or(Value::Null, float_value));
        m.insert("references".into(), prob.refs.clone());
        m.insert("penalties_at_start".into(), prob.penalties_at_start.clone());
        m.insert("start_state".into(), prob.start_state.clone());
        m.insert("scaling".into(), self.spec.settings.get("scaling").cloned().unwrap_or(Value::Null));
        m.insert("steerable".into(), json!(self.steerable));
        m.insert("occupancy".into(), json!(self.spec.occupancy));
        m.insert("driver".into(), json!(self.driver_kind));
        m.insert("mma".into(), mma.map_or(Value::Null, MmaDriver::describe));
        m.insert("lr_scale".into(), float_value(self.lr_scale));
        m.insert("retreats".into(), Value::Array(self.retreats.clone()));
        m.insert("regimes".into(), json!(self.regime + 1));
        m.insert("model_timeline".into(), Value::Array(self.timeline.clone()));
        let skip = self.push_stats.len().saturating_sub(200);
        m.insert("push_stats".into(), Value::Array(self.push_stats[skip..].to_vec()));
        m.insert("history".into(), Value::Array(self.hist.clone()));
        write_json_indent1(&self.out_dir.join("history.json"), &Value::Object(m))
    }
}

struct Prev {
    z: NamedArrays,
    gz: NamedArrays,
    opt_state: AdamState,
    f0: f64,
    meas: Option<Measurement>,
    warm: Option<Warm>,
    mma: Option<Snapshot>,
}


#[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
pub fn solve(
    spec: &Arc<OptimizeSpec>,
    out_dir: &Path,
    resume: bool,
    emit: Emit<'_>,
    log: LogFn<'_>,
    replay: Option<&[Value]>,
) -> JobResult<SolveOutcome> {
    std::fs::create_dir_all(out_dir)?;
    let t_run0 = crate::private::epoch_seconds();
    let stage = |frac: f64, msg: &str| {
        emit("progress", &json!({"frac": float_value(frac), "message": msg}));
        log(&format!("  .. {msg}"));
    };
    stage(0.0, "building the problem: the physics case, the frozen physics state, the model's occupancy");
    let mut prob = Problem::new(spec, Some(log), false)?;
    let design =
        Design::new(&spec.free, spec.settings.get("scaling").and_then(Value::as_str).unwrap_or("unit_range"));
    let driver_kind = select_optimizer(spec, &prob.constraints)?;
    let penalise = driver_kind == "adam";
    let adam = Adam::default();
    let lr = super::spec::py_float(&spec.settings["lr"])?;
    let max_retries = spec.int_setting("nonfinite_retries");
    let mut mma = if driver_kind == "mma" { Some(MmaDriver::new(&prob, &design)?) } else { None };
    let phys_cons: Vec<usize> = mma.as_ref().map(|m| m.physics_idx.clone()).unwrap_or_default();
    let n_iter = spec.int_setting("iters");
    let live_every = spec.int_setting("live_every");
    let steerable = spec.settings.get("steerable").is_some_and(implexity_core::pyobj::truthy);
    let drive = model_drive_of(spec)?;

    log(&"=".repeat(72));
    log(&format!(
        "IMPLICIT OPTIMISATION -- {} [{}]",
        py_str(spec.bbox.get("name").unwrap_or(&Value::Null)),
        spec.physics.name()
    ));
    log(&format!(
        "model  {}  structure {}  content {}  geometry representation {}",
        spec.model.kind(),
        spec.model.structure_id(),
        spec.model.content_id(),
        prob.classification().text()
    ));
    let bound = |v: Option<f64>| v.map_or_else(|| "None".to_string(), implexity_core::py_repr::repr_float);
    log(&format!(
        "free   {}",
        spec.free
            .iter()
            .map(|f| format!(
                "{} [{}] in [{}, {}], {} value(s)",
                f.ref_str(),
                f.units,
                bound(f.lo),
                bound(f.hi),
                f.size
            ))
            .collect::<Vec<_>>()
            .join("; ")
    ));
    log(&format!(
        "grid   {} (h = {} mm); model occupies {} of the box, {} of {} element centres in the band ({} occupancy)",
        repr(spec.bbox.get("grid").unwrap_or(&Value::Null)),
        fmt_f(super::spec::py_float(spec.bbox.get("h_mm").unwrap_or(&Value::Null)).unwrap_or(f64::NAN), 4),
        fmt_f(prob.probe.get("V_model").and_then(Value::as_f64).unwrap_or(f64::NAN), 4),
        py_str(prob.probe.get("band_cells").unwrap_or(&Value::Null)),
        py_str(prob.probe.get("cells").unwrap_or(&Value::Null)),
        spec.occupancy
    ));
    let terms_text = spec
        .objective
        .get("terms")
        .and_then(Value::as_array)
        .map(|t| {
            t.iter()
                .map(|x| {
                    format!(
                        "{} x {}",
                        x.get("term").map(py_str).unwrap_or_default(),
                        g(x.get("weight").and_then(Value::as_f64).unwrap_or(f64::NAN))
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let cons_text = if prob.constraints.is_empty() {
        String::new()
    } else {
        format!(
            "  | {}",
            prob.constraints
                .iter()
                .map(|c| format!("{} -> {}", c.kind(), g(c.resolved().unwrap_or(f64::NAN))))
                .collect::<Vec<_>>()
                .join("; ")
        )
    };
    log(&format!("obj    {terms_text}{cons_text}"));
    let scaling = spec.settings.get("scaling").and_then(Value::as_str).unwrap_or("unit_range").to_string();
    match &mma {
        None => log(&format!(
            "schedule {n_iter} iterations, adam, lr {} ({}), scaling {scaling}, steerable {}",
            g(lr),
            if scaling == "unit_range" { "fraction of each parameter's declared range" } else { "RAW UNITS" },
            if steerable { "True" } else { "False" }
        )),
        Some(m) => log(&format!(
            "schedule {n_iter} iterations, {}, move limit {} of each parameter's declared range, {} constraint(s) \
             held as inequalities (the penalty weights do not apply), scaling {scaling}, steerable {}",
            if m.opts.get("globalize").and_then(Value::as_bool).unwrap_or(true) { "GCMMA" } else { "MMA" },
            g(m.move_limit()),
            m.m(),
            if steerable { "True" } else { "False" }
        )),
    }

    let mut st = Loop {
        spec: Arc::clone(spec),
        out_dir,
        log,
        t_run0,
        n_iter,
        steerable,
        driver_kind: driver_kind.clone(),
        hist: Vec::new(),
        best: Best::empty(Value::Null, Value::Null),
        push_stats: Vec::new(),
        timeline: Vec::new(),
        retreats: Vec::new(),
        regime: 0,
        lr_scale: 1.0,
        drive,
        steer_queue: Vec::new(),
    };
    let mut z: NamedArrays;
    let mut warm: Option<Warm>;
    let mut opt_state: AdamState;
    let start_it: i64;
    if resume {
        let ck = implexity_optim::optjob::load_checkpoint(out_dir).map_err(cae)?;
        z = ck.params.clone();
        warm = prob.warm_from(&ck.warm)?;
        let extras = ck.extras.clone();
        prob.restore(ck.l0, &ck.refs, extras.get("physics").unwrap_or(&Value::Null))?;
        st.lr_scale = extras.get("lr_scale").and_then(Value::as_f64).unwrap_or(1.0);
        start_it = ck.it_next;
        let hp = out_dir.join("history.json");
        if hp.is_file() {
            let old = implexity_core::json::read_file(&hp).map_err(JobError::runtime)?;
            let n = usize::try_from(ck.n_rows).unwrap_or(0);
            st.hist = old
                .get("history")
                .and_then(Value::as_array)
                .map(|h| h.iter().take(n).cloned().collect())
                .unwrap_or_default();
            st.push_stats = old.get("push_stats").and_then(Value::as_array).cloned().unwrap_or_default();
            st.timeline = old.get("model_timeline").and_then(Value::as_array).cloned().unwrap_or_default();
            st.retreats = old.get("retreats").and_then(Value::as_array).cloned().unwrap_or_default();
            prob.penalties_at_start = old.get("penalties_at_start").cloned().unwrap_or(Value::Null);
            prob.start_state = old.get("start_state").cloned().unwrap_or(Value::Null);
            st.regime = i64::try_from(
                st.timeline
                    .iter()
                    .filter(|e| e.get("applied").is_some_and(implexity_core::pyobj::truthy))
                    .count(),
            )
            .unwrap_or(0);
        }
        st.best = best_of_rows(&st.hist, &driver_kind);
        let bp = out_dir.join("best.npz");
        if st.best.fields.contains_key("i") && bp.is_file() {
            let (marker, values) = read_design(&bp)?;
            if marker.map(Value::from) == st.best.fields.get("i").cloned() {
                let slot_of: std::collections::BTreeMap<String, String> =
                    spec.free.iter().map(|f| (f.ref_str(), f.slot.clone())).collect();
                let mut p = NamedArrays::new();
                for (r, v) in values {
                    let slot = slot_of
                        .get(&r)
                        .ok_or_else(|| JobError::of("KeyError", implexity_core::py_repr::repr_str(&r)))?;
                    p.insert(slot.clone(), v);
                }
                st.best.p = Some(p);
            } else {
                log(
                    "best.npz does not carry the best row of the final regime; the best design is re-established by the rows that follow",
                );
                st.best = Best::empty(st.best.get("regime_key"), st.best.get("first_L"));
                st.best.fields.insert("regime_key".into(), st.best.get("regime_key"));
                st.best.fields.insert("first_L".into(), st.best.get("first_L"));
            }
        }
        if steerable {
            let sp = out_dir.join("drive.json");
            if sp.is_file() {
                let stored = implexity_core::json::read_file(&sp).map_err(JobError::runtime)?;
                if let Some(m) = stored.as_object() {
                    for (k, v) in m {
                        if st.drive.contains_key(k)
                            && let Some(a) = json_array(v)
                        {
                            st.drive.insert(k.clone(), a);
                        }
                    }
                }
            }
        }
        opt_state = ck.adam.clone().unwrap_or_else(|| adam.init(&z));
        if let Some(m) = mma.as_mut() {
            m.load(out_dir, &|s| log(s))?;
        }
        log(&format!(
            "RESUMED from ckpt: it {start_it}, {} rows, L0 = {}, step scale {}",
            st.hist.len(),
            fmt_f(prob.l0.unwrap_or(f64::NAN), 9),
            g(st.lr_scale)
        ));
    } else {
        stage(0.0, "calibrating the objective on the start model");
        let (l0, refs) = prob.calibrate()?;
        log(&format!("calibration L0 = {}  refs {}", fmt_f(l0, 9), repr(&refs)));
        if let Some(pen) = prob.penalties_at_start.as_object().filter(|m| !m.is_empty()) {
            log(&format!(
                "admissibility figures on the frozen physics (constants of this run, not part of L): {}",
                pen.iter()
                    .map(|(k, v)| format!(
                        "{k} = {}",
                        fmt_g(v.get("value").and_then(Value::as_f64).unwrap_or(f64::NAN), 4)
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let (z0, _) = design.project(&design.z_of(&design.start()));
        z = z0;
        opt_state = adam.init(&z);
        warm = prob.warm0.clone();
        start_it = 0;
    }
    let mut queue: Vec<Value> = replay.map(<[Value]>::to_vec).unwrap_or_default();
    queue.sort_by_key(|e| e.get("at_iteration").and_then(|v| super::spec::py_int(v).ok()).unwrap_or(0));
    st.steer_queue = queue;

    let save_checkpoint = |st: &Loop<'_>,
                           prob: &Problem,
                           mma: Option<&MmaDriver>,
                           z: &NamedArrays,
                           opt_state: &AdamState,
                           warm: Option<&Warm>,
                           it_next: i64,
                           n_rows: usize|
     -> JobResult<(f64, u64)> {
        let warm_arrays = prob.warm_arrays(warm);
        let extras = json!({"physics": prob.physics.extras(), "lr_scale": float_value(st.lr_scale)});
        let out = implexity_optim::optjob::save_checkpoint(
            out_dir,
            &CheckpointInput {
                params: z,
                adam: Some(opt_state),
                warm: &warm_arrays,
                it_next,
                l0: prob.l0.unwrap_or(f64::NAN),
                refs: &prob.refs,
                n_rows: i64::try_from(n_rows).unwrap_or(i64::MAX),
                extras: &extras,
                stage_idx: 0,
            },
        )
        .map_err(cae)?;
        if let Some(m) = mma {
            m.save(out_dir)?;
        }
        Ok(out)
    };

    stage(
        0.0,
        &match &mma {
            None => "tracing and compiling the gradient (one XLA compilation; tens of seconds, and the only one this run pays)".to_string(),
            Some(m) => format!(
                "tracing and compiling the gradient (one XLA compilation of the physics) and {} occupancy constraint gradient(s)",
                m.occupancy_idx.len()
            ),
        },
    );
    let mut it = start_it;
    let mut retries: i64 = 0;
    let mut prev: Option<Prev> = None;
    let mut stopped_early = false;
    while it < n_iter {

        if let Some(op) =
            implexity_optim::optjob::read_control(out_dir).filter(|o| o == "pause" || o == "stop")
        {
            save_checkpoint(&st, &prob, mma.as_ref(), &z, &opt_state, warm.as_ref(), it, st.hist.len())?;
            st.write_history(if op == "pause" { "paused" } else { "stopped" }, &prob, mma.as_ref())?;
            log(&format!("halted [{op}] at it {it} ({} iterations recorded)", st.hist.len()));
            emit("halted", &json!({"op": op, "iterations": st.hist.len()}));
            return Err(JobError::of(HALTED, op));
        }
        let mut steered_here: Option<Value> = None;
        while let Some(req) = st.pending(it) {

            let t0 = Instant::now();
            let mut ent = Map::new();
            ent.insert("seq".into(), json!(st.timeline.len() + 1));
            ent.insert("at_iteration".into(), json!(it));
            ent.insert("t_wall".into(), float_value(round3(crate::private::epoch_seconds() - st.t_run0)));
            ent.insert(
                "replayed".into(),
                json!(req.get("_replayed").is_some_and(implexity_core::pyobj::truthy)),
            );
            ent.insert("request_id".into(), req.get("request_id").cloned().unwrap_or(Value::Null));
            if steerable {
                let d = classify_model_steer(spec, &st.drive, &Value::Object(req.clone()));
                for (k, v) in d.as_dict() {
                    ent.insert(k, v);
                }
                if d.refused.is_some() {
                    ent.insert("applied".into(), json!(false));
                } else {
                    for (k, v) in &d.sets {
                        st.drive.insert(k.clone(), v.clone());
                    }
                    let mom = req
                        .get("momentum")
                        .filter(|v| implexity_core::pyobj::truthy(v))
                        .or_else(|| spec.settings.get("momentum"))
                        .map(py_str)
                        .unwrap_or_default()
                        .to_lowercase();
                    if mom == "reset" {
                        opt_state = adam.init(&z);
                        if let Some(m) = mma.as_mut() {
                            m.reset()?;
                        }
                    }
                    st.regime += 1;
                    ent.insert("applied".into(), json!(true));
                    ent.insert("regime".into(), json!(st.regime));
                    ent.insert("momentum".into(), json!(mom));
                    ent.insert(
                        "rebuilt".into(),
                        json!(["model drive vector only (no re-trace, no XLA recompilation)"]),
                    );
                    let text = implexity_core::json::dumps(
                        &drive_value(&st.drive),
                        &implexity_core::json::DumpOptions::default(),
                    );
                    std::fs::write(out_dir.join("drive.json.tmp"), text)?;
                    std::fs::rename(out_dir.join("drive.json.tmp"), out_dir.join("drive.json"))?;
                }
            } else {
                ent.insert("applied".into(), json!(false));
                ent.insert(
                    "refused".into(),
                    json!("this job was not started with steerable=True: the model's fixed parameters are constants \
                           closed over by the compiled gradient, so changing one costs a re-trace.  Restart with \
                           steerable=True."),
                );
            }
            ent.insert("steer_ms".into(), float_value(round3(t0.elapsed().as_secs_f64() * 1e3)));
            let applied = ent.get("applied").is_some_and(implexity_core::pyobj::truthy);
            let changes: Vec<String> = ent
                .get("changes")
                .and_then(Value::as_array)
                .map(|c| c.iter().map(py_str).collect())
                .unwrap_or_default();
            let detail = if changes.is_empty() {
                ent.get("refused").filter(|v| !v.is_null()).map(py_str).unwrap_or_default()
            } else {
                changes.join("; ")
            };
            log(&format!(
                "  STEER #{} {} at i {it}: {detail}",
                py_str(&ent["seq"]),
                if applied { "applied" } else { "REFUSED" }
            ));
            let ent = Value::Object(ent);
            st.timeline.push(ent.clone());
            emit("steer", &ent);
            if applied {
                steered_here = ent.get("seq").cloned();
                prev = None;
            }
        }
        let t_it0 = Instant::now();
        let p = design.p_of(&z);
        let drive_opt = if steerable { Some(st.drive.clone()) } else { None };
        let evaluated = prob.evaluate(&p, warm.as_ref(), drive_opt.as_ref(), penalise, true, &phys_cons);
        let mut failure: Option<String> = None;
        let mut ev = None;
        match evaluated {
            Ok(e) => {
                let grads_ok = e.grad.as_ref().is_none_or(implexity_optim::optjob::all_finite)
                    && e.physics_residual_grads.iter().all(implexity_optim::optjob::all_finite)
                    && e.physics_residuals.iter().all(|v| v.is_finite());
                if e.total.is_finite() && all_finite_value(&Value::Object(e.aux.clone())) && grads_ok {
                    ev = Some(e);
                } else {
                    failure = Some("non-finite objective, physics state or gradient".into());
                }
            }
            Err(e) if e.solver_recovery().is_some() => {
                st.write_history("error", &prob, mma.as_ref())?;
                return Err(e);
            }
            Err(e) if is_numerical_failure(&e) && !super::problem::is_capability_refusal(&e) => {
                failure = Some(failure_text(&e));
            }
            Err(e) => return Err(e),
        }
        if let Some(failure) = failure {
            let Some(pv) = prev.as_ref().filter(|_| retries < max_retries) else {
                st.write_history("error", &prob, mma.as_ref())?;
                return Err(JobError::optimize1(format!(
                    "the physics evaluation failed at iteration {it} ({failure}){}",
                    if prev.is_none() {
                        String::new()
                    } else {
                        format!(" and still failed after {retries} step-halving retreat(s)")
                    }
                )));
            };
            retries += 1;
            st.lr_scale *= 0.5;
            if let Some(m) = mma.as_mut() {
                if let Some(snap) = &pv.mma {
                    m.restore_snapshot(snap)?;
                }
                m.shrink(0.5);
            }
            let (z_next, state_next, _) = descend(
                &mut prob,
                mma.as_mut(),
                &design,
                &adam,
                lr * st.lr_scale,
                &pv.z,
                &pv.gz,
                &pv.opt_state,
                pv.f0,
                pv.meas.as_ref(),
                pv.warm.as_ref(),
                drive_opt.as_ref(),
                &format!("retreat before it {it}"),
                &phys_cons,
                log,
            )?;
            let (zp, _) = design.project(&z_next);
            z = zp;
            opt_state = state_next;
            warm.clone_from(&pv.warm);
            let ev_row = json!({
                "at_iteration": it, "reason": failure, "retry": retries, "lr_scale": float_value(st.lr_scale),
                "move_limit": mma.as_ref().map_or(Value::Null, |m| float_value(m.move_limit())),
            });
            st.retreats.push(ev_row.clone());
            log(&format!(
                "  RETREAT at it {it}: {failure}; step halved (scale {}), retry {retries}/{max_retries}",
                g(st.lr_scale)
            ));
            emit("retreat", &ev_row);
            save_checkpoint(&st, &prob, mma.as_ref(), &z, &opt_state, warm.as_ref(), it, st.hist.len())?;
            st.write_history("running", &prob, mma.as_ref())?;
            continue;
        }
        let Some(ev) = ev else { continue };
        retries = 0;
        warm.clone_from(&ev.warm);
        let gp = ev.grad.clone().unwrap_or_default();
        let gz = design.grad_z(&gp);
        let v_model = ev.aux.get("V_model").and_then(Value::as_f64).unwrap_or(f64::NAN);
        let v_target = prob
            .constraints
            .iter()
            .find(|c| c.kind() == "volume_fraction")
            .and_then(super::spec::Constraint::resolved);
        let mut row = Map::new();
        row.insert("i".into(), json!(it));
        row.insert("stage".into(), json!(0));
        row.insert("it".into(), json!(it));
        row.insert("L".into(), float_value(ev.total));
        row.insert("L_physical".into(), ev.aux.get("L_physical").cloned().unwrap_or(Value::Null));
        row.insert("penalty".into(), float_value(0.0));
        row.insert("V".into(), float_value(v_model));
        row.insert(
            "dV".into(),
            float_value(match v_target {
                Some(t) if t != 0.0 => (v_model - t).abs(),
                _ => 0.0,
            }),
        );
        row.insert("projection".into(), json!("bounds"));
        row.insert(
            "constraint_penalty".into(),
            ev.aux.get("constraint_penalty").cloned().unwrap_or(Value::Null),
        );
        row.insert("regime".into(), json!(st.regime));
        row.insert("terms".into(), prob.physics.term_row(&Value::Object(ev.aux.clone())));
        row.insert("driver".into(), json!(driver_kind));
        row.insert("lr_scale".into(), float_value(st.lr_scale));
        for (k, v) in prob.row_quantities(&ev.aux) {
            row.insert(k, v);
        }
        let mut meas = None;
        if let Some(m) = mma.as_ref() {
            let me = m.measure(
                &prob,
                &z,
                drive_opt.as_ref(),
                Some((&ev.physics_residuals, &ev.physics_residual_grads)),
            )?;
            let rows: Vec<Value> = m
                .constraints
                .iter()
                .zip(&me.fs)
                .map(|(c, gv)| {
                    let mut d = c.describe().as_object().cloned().unwrap_or_default();
                    d.insert("residual".into(), float_value(*gv));
                    Value::Object(d)
                })
                .collect();
            row.insert("constraints".into(), Value::Array(rows));
            row.insert("feasible".into(), json!(me.feasible));
            meas = Some(me);
        }
        if let Some(seq) = &steered_here {
            row.insert("steer_seq".into(), seq.clone());
            row.insert("steer_applied_before".into(), json!(true));
        }
        let mut free_rows = Map::new();
        for fr in &spec.free {
            let pv = p.get(&fr.slot).cloned().unwrap_or_else(|| ArrayD::zeros(IxDyn(&[])));
            let gv = gp.get(&fr.slot).cloned().unwrap_or_else(|| ArrayD::zeros(IxDyn(&[])));
            free_rows.insert(
                fr.ref_str(),
                json!({
                    "units": fr.units, "value": scalar_or_null(&pv),
                    "mean": float_value(implexity_optim::numeric::array_mean(&pv)),
                    "min": float_value(amin(&pv)), "max": float_value(amax(&pv)),
                    "grad_absmax": float_value(abs_max(&gv)), "grad_absmean": float_value(abs_mean(&gv)),
                    "grad_zeros": gv.iter().filter(|x| **x == 0.0).count(), "size": pv.len(),
                }),
            );
        }
        row.insert("free".into(), Value::Object(free_rows));
        if !implexity_core::pyobj::py_eq(&st.best.get("regime_key"), &json!(st.regime))
            || !st.best.fields.contains_key("regime_key")
        {
            st.best = Best::empty(json!(st.regime), float_value(ev.total));
            st.best.fields.insert("regime_key".into(), json!(st.regime));
            st.best.fields.insert("first_L".into(), float_value(ev.total));
        }
        if ev.total < st.best.l() && meas.as_ref().is_none_or(|m| m.feasible) {
            let first_l = st.best.get("first_L");
            let mut f = row.clone();
            f.insert("regime_key".into(), json!(st.regime));
            f.insert("first_L".into(), first_l);
            st.best = Best { fields: f, p: Some(p.clone()) };
            write_design(out_dir, "best.npz", spec, &p, Some(it))?;
        }
        prev = Some(Prev {
            z: z.clone(),
            gz: gz.clone(),
            opt_state: opt_state.clone(),
            f0: ev.total,
            meas: meas.clone(),
            warm: warm.clone(),
            mma: mma.as_ref().map(MmaDriver::snapshot),
        });
        let (z_next, state_next, mrow) = descend(
            &mut prob,
            mma.as_mut(),
            &design,
            &adam,
            lr * st.lr_scale,
            &z,
            &gz,
            &opt_state,
            ev.total,
            meas.as_ref(),
            warm.as_ref(),
            drive_opt.as_ref(),
            &format!("it {it}"),
            &phys_cons,
            log,
        )?;
        opt_state = state_next;
        let mut stop_early = false;
        if let Some(mrow) = mrow {
            let kkt = mrow.get("kkt_residual").cloned().unwrap_or(Value::Null);
            if let (Some(Value::Array(rows)), Some(Value::Array(lams))) =
                (row.get_mut("constraints"), mrow.get("multipliers").cloned())
            {
                for (cr, lam) in rows.iter_mut().zip(lams) {
                    if let Some(o) = cr.as_object_mut() {
                        o.insert("multiplier".into(), lam);
                    }
                }
            }
            row.insert("mma".into(), Value::Object(mrow));
            row.insert("kkt_residual".into(), kkt);
            stop_early = mma.as_ref().is_some_and(MmaDriver::converged);
        }
        let (zp, active) = design.project(&z_next);
        z = zp;
        row.insert(
            "at_bound".into(),
            Value::Object(
                spec.free
                    .iter()
                    .map(|f| (f.ref_str(), json!(active.get(&f.slot).copied().unwrap_or(0))))
                    .collect(),
            ),
        );
        row.insert("wall_s".into(), float_value(round3(t_it0.elapsed().as_secs_f64())));
        let mut push = Value::Null;
        if live_every != 0 && (it % live_every == 0 || it == n_iter - 1) {
            let (ms, nbytes) = write_design(out_dir, "live_model.npz", spec, &design.p_of(&z), None)?;
            let ms = implexity_mesh::numeric::py_round_digits(ms, 2);
            push = json!({"file": "live_model.npz", "bytes": nbytes, "save_ms": float_value(ms)});
            st.push_stats.push(json!({"i": it, "bytes": nbytes, "save_ms": float_value(ms)}));
        }
        row.insert("push".into(), push);
        let (ck_ms, _) = save_checkpoint(
            &st,
            &prob,
            mma.as_ref(),
            &z,
            &opt_state,
            warm.as_ref(),
            it + 1,
            st.hist.len() + 1,
        )?;
        row.insert("ckpt_ms".into(), float_value(implexity_mesh::numeric::py_round_digits(ck_ms, 2)));
        let row_v = Value::Object(row.clone());
        st.hist.push(row_v.clone());
        st.write_history("running", &prob, mma.as_ref())?;
        emit("iter", &row_v);
        let quantities: Vec<String> = spec
            .physics
            .row_quantities()
            .into_iter()
            .filter_map(|[k, _, _, _]| {
                row.get(&k).and_then(Value::as_f64).map(|v| format!("{k}={}", fmt_g(v, 4)))
            })
            .collect();
        let frees: Vec<String> = spec
            .free
            .iter()
            .map(|f| {
                let r = f.ref_str();
                let short = r.rsplit(':').next().unwrap_or("").to_string();
                let mean = row["free"][&r]["mean"].as_f64().unwrap_or(f64::NAN);
                format!("{short}={}", fmt_g(mean, 4))
            })
            .collect();
        let tail = if mma.is_some() {
            let gmax =
                row.get("constraints").and_then(Value::as_array).filter(|c| !c.is_empty()).map_or_else(
                    || "-".to_string(),
                    |c| {
                        fmt_g(
                            c.iter().filter_map(|x| x["residual"].as_f64()).fold(f64::NEG_INFINITY, f64::max),
                            3,
                        )
                    },
                );
            let lams: Vec<String> = row["mma"]["multipliers"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|v| implexity_core::py_repr::repr_str(&fmt_g(v.as_f64().unwrap_or(f64::NAN), 3)))
                        .collect()
                })
                .unwrap_or_default();
            let conservative = row["mma"]["conservative"].as_bool().unwrap_or(true);
            format!(
                "  | kkt {}  g_max {gmax}  lam [{}]{}",
                fmt_g(row["kkt_residual"].as_f64().unwrap_or(f64::NAN), 3),
                lams.join(", "),
                if conservative {
                    String::new()
                } else {
                    format!(
                        "  (NOT conservative after {} re-solves)",
                        py_str(&row["mma"]["inner_iterations"])
                    )
                }
            )
        } else {
            String::new()
        };
        log(&format!(
            "  it {it}  L = {}  V = {}  {}  [{} s, ckpt {} ms]  {}{tail}",
            fmt_f(ev.total, 6),
            fmt_f(v_model, 6),
            quantities.join(" "),
            fmt_f(row["wall_s"].as_f64().unwrap_or(0.0), 2),
            fmt_f(row["ckpt_ms"].as_f64().unwrap_or(0.0), 0),
            frees.join(" ")
        ));
        it += 1;
        if stop_early {
            stopped_early = true;
            log(&format!(
                "  MMA outer KKT residual {} is below kkt_tol {} at it {}; the schedule had {} more iteration(s)",
                fmt_g(row["kkt_residual"].as_f64().unwrap_or(f64::NAN), 3),
                g(mma
                    .as_ref()
                    .and_then(|m| m.opts.get("kkt_tol"))
                    .and_then(Value::as_f64)
                    .unwrap_or(f64::NAN)),
                it - 1,
                n_iter - it
            ));
            break;
        }
    }

    let drive_end = if steerable { Some(st.drive.clone()) } else { None };
    let mut classification_end: Option<Classification> = None;
    let mut fc_move: Option<String> = None;
    match prob.model_of(&design.p_of(&z), drive_end.as_ref()).and_then(|m| prob.bridge.check(&m)) {
        Ok(c) => {
            let start_direct = prob.geometry_representation.is_some();
            let end_direct = c.is_direct();
            if start_direct != end_direct {
                fc_move = Some(format!(
                    "the model's geometry representation changed during the run; start={}, final={}",
                    prob.classification().text(),
                    c.text()
                ));
            } else if let Classification::Field(fc) = &c
                && !start_direct
                && (fc.safe_step_factor().unwrap_or(0.0) - prob.step_factor.unwrap_or(f64::NAN)).abs() > 1e-12
            {
                fc_move = Some(format!(
                    "the model's field class moved during the run: the occupancy band was measured with the START \
                     model's conversion {} and the final model's is {}.  The volumes this run reports are banded at \
                     the start factor.",
                    fmt_g(prob.step_factor.unwrap_or(f64::NAN), 9),
                    fmt_g(fc.safe_step_factor().unwrap_or(0.0), 9)
                ));
            }
            if let Some(w) = &fc_move {
                log(&format!("WARNING: {w}"));
            }
            classification_end = Some(c);
        }
        Err(e) => log(&format!("final field class not re-measured: {}", e.message())),
    }
    let elapsed = crate::private::epoch_seconds() - t_run0;
    let best_found = st.best.p.is_some();
    if !best_found {
        log(&format!(
            "no {} row of the final regime; the final iterate is reported as best and flagged",
            if mma.is_some() { "feasible" } else { "scored" }
        ));
        let mut f = st.hist.last().and_then(Value::as_object).cloned().unwrap_or_default();
        f.insert("first_L".into(), st.best.get("first_L"));
        st.best = Best { fields: f, p: Some(design.p_of(&z)) };
    }
    let best_p = st.best.p.clone().unwrap_or_default();
    write_design(out_dir, "final_model.npz", spec, &design.p_of(&z), None)?;
    write_design(out_dir, "best.npz", spec, &best_p, st.best.fields.get("i").and_then(Value::as_i64))?;
    let section = (|| -> JobResult<()> {
        let dm = prob.occupancy(&best_p, drive_end.as_ref())?;
        let rho = ArrayD::from_shape_vec(IxDyn(&prob.bridge.shape), dm).map_err(cae)?;
        let png = implexity_optim::optjob::box_section_png(&rho, 2, 24).map_err(cae)?;
        std::fs::write(out_dir.join("section_zmid.png"), png)?;
        Ok(())
    })();
    if let Err(e) = section {
        log(&format!("section image skipped: {}", e.message()));
    }
    let rep = prob.classification().report();
    let rep_end = classification_end.as_ref().map(Classification::report);
    let peak = implexity_optim::optjob::peak_rss_mb();
    let mut s = Map::new();
    s.insert("status".into(), json!("completed"));
    s.insert("kind".into(), json!("implicit_optimize"));
    s.insert("physics".into(), json!(spec.physics.name()));
    s.insert("case".into(), spec.bbox.get("name").cloned().unwrap_or(Value::Null));
    s.insert("grid".into(), spec.bbox.get("grid").cloned().unwrap_or(Value::Null));
    s.insert("h_mm".into(), spec.bbox.get("h_mm").cloned().unwrap_or(Value::Null));
    s.insert("iterations".into(), json!(st.hist.len()));
    s.insert("elapsed_s".into(), float_value(implexity_mesh::numeric::py_round_digits(elapsed, 1)));
    s.insert("L_first".into(), st.hist.first().and_then(|r| r.get("L")).cloned().unwrap_or(Value::Null));
    s.insert("L_best".into(), st.best.get("L"));
    s.insert("L_last".into(), st.hist.last().and_then(|r| r.get("L")).cloned().unwrap_or(Value::Null));
    s.insert("L_regime_first".into(), st.best.get("first_L"));
    let first_l = st.best.get("first_L");
    s.insert(
        "L_decreased".into(),
        json!(best_found && !first_l.is_null() && st.best.l() < first_l.as_f64().unwrap_or(f64::NAN)),
    );
    s.insert(
        "L_decreased_note".into(),
        json!("best L of the final regime against that regime's first row; an applied steer starts a new problem"),
    );
    s.insert("best_found".into(), json!(best_found));
    s.insert("spec".into(), spec.describe());
    s.insert("solve_id".into(), json!(spec.digest()));
    s.insert("lattice".into(), prob.lattice());
    s.insert("probe".into(), Value::Object(prob.probe.clone()));
    s.insert("l0".into(), prob.l0.map_or(Value::Null, float_value));
    s.insert("references".into(), prob.refs.clone());
    s.insert("penalties_at_start".into(), prob.penalties_at_start.clone());
    s.insert("start_state".into(), prob.start_state.clone());
    s.insert("scaling".into(), json!(scaling));
    s.insert("steerable".into(), json!(steerable));
    s.insert("occupancy".into(), json!(spec.occupancy));
    s.insert("field_class".into(), prob.field_class.as_ref().map_or(Value::Null, |fc| json!(fc.repr())));
    s.insert("step_factor".into(), rep["step_factor"].clone());
    s.insert("geometry_representation".into(), rep["geometry_representation"].clone());
    s.insert(
        "field_class_end".into(),
        match &classification_end {
            Some(Classification::Field(fc)) => json!(fc.repr()),
            _ => Value::Null,
        },
    );
    s.insert(
        "geometry_representation_end".into(),
        rep_end.map_or(Value::Null, |r| r["geometry_representation"].clone()),
    );
    s.insert("field_class_warning".into(), json!(fc_move));
    s.insert("driver".into(), json!(driver_kind));
    s.insert("lr_scale_final".into(), float_value(st.lr_scale));
    s.insert("retreats".into(), Value::Array(st.retreats.clone()));
    s.insert(
        "mma".into(),
        match &mma {
            None => Value::Null,
            Some(m) => {
                let mut d = m.describe().as_object().cloned().unwrap_or_default();
                d.insert("kkt_residual_last".into(), m.last_kkt().map_or(Value::Null, float_value));
                d.insert("nonfinite_trials".into(), json!(m.nonfinite_trials()));
                d.insert("stopped_early".into(), json!(stopped_early));
                d.insert(
                    "best_feasible".into(),
                    json!(st.best.fields.get("feasible").is_some_and(implexity_core::pyobj::truthy)),
                );
                Value::Object(d)
            }
        },
    );
    s.insert("regimes".into(), json!(st.regime + 1));
    s.insert("model_timeline".into(), Value::Array(st.timeline.clone()));
    s.insert(
        "free_final".into(),
        Value::Object(
            spec.free.iter().map(|f| (f.ref_str(), best_p.get(&f.slot).map_or(Value::Null, safe))).collect(),
        ),
    );
    s.insert(
        "free_start".into(),
        Value::Object(spec.free.iter().map(|f| (f.ref_str(), safe(&f.start))).collect()),
    );
    s.insert(
        "peak_rss_mb".into(),
        if peak.is_finite() {
            float_value(implexity_mesh::numeric::py_round_digits(peak, 1))
        } else {
            Value::Null
        },
    );
    let skip = st.push_stats.len().saturating_sub(200);
    s.insert("push_stats".into(), Value::Array(st.push_stats[skip..].to_vec()));
    s.insert("history".into(), Value::Array(st.hist.clone()));
    write_json_indent1(&out_dir.join("summary.json"), &Value::Object(s.clone()))?;
    st.write_history("completed", &prob, mma.as_ref())?;
    let mut done = s.clone();
    done.remove("history");
    emit("done", &Value::Object(done));
    log(&format!(
        "done: {} iterations, {} s; L {} -> {} (best {}); {} retreat(s)",
        st.hist.len(),
        fmt_f(elapsed, 1),
        py_str(&s["L_first"]),
        py_str(&s["L_last"]),
        fmt_f(st.best.l(), 6),
        st.retreats.len()
    ));
    Ok(SolveOutcome { summary: s, best: best_p })
}

#[allow(clippy::too_many_arguments)]
fn descend(
    prob: &mut Problem,
    mma: Option<&mut MmaDriver>,
    design: &Design,
    adam: &Adam,
    step: f64,
    z_at: &NamedArrays,
    gz_at: &NamedArrays,
    state_at: &AdamState,
    f0_at: f64,
    meas_at: Option<&Measurement>,
    warm_at: Option<&Warm>,
    drive: Option<&Drive>,
    where_: &str,
    phys_cons: &[usize],
    log: LogFn<'_>,
) -> JobResult<(NamedArrays, AdamState, Option<Map<String, Value>>)> {
    let Some(mma) = mma else {
        let (dirn, next) = adam.direction(gz_at, state_at);
        let z_next = NamedArrays::from_pairs(z_at.iter().map(|(k, v)| {
            let d = dirn.get(k).cloned().unwrap_or_else(|| ArrayD::zeros(v.raw_dim()));
            (k.to_string(), v - &(d * step))
        }));
        return Ok((z_next, next, None));
    };
    let meas = meas_at.ok_or_else(|| JobError::runtime("MMA step without a measurement"))?;
    let occ_idx = mma.occupancy_idx.clone();
    let mut eval_true = |zt: &NamedArrays| -> (f64, Vec<f64>, Vec<f64>) {
        let pt = design.p_of(zt);
        let occ: Vec<f64> = occ_idx
            .iter()
            .map(|j| prob.occupancy_residual(*j, &pt, drive, false).map_or(f64::NAN, |(v, _)| v))
            .collect();
        match prob.evaluate(&pt, warm_at, drive, false, false, phys_cons) {
            Ok(ev) => (ev.total, occ, ev.physics_residuals),
            Err(e) => {
                log(&format!(
                    "  GCMMA trial evaluation failed ({}); treated as non-conservative",
                    e.python_class()
                ));
                (f64::NAN, occ, vec![f64::NAN; phys_cons.len()])
            }
        }
    };
    let (z_next, info) = mma.step(z_at, f0_at, gz_at, meas, where_, &mut eval_true, &|s| log(s))?;
    Ok((z_next, state_at.clone(), Some(info)))
}
