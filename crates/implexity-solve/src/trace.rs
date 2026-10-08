// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END





use std::cell::RefCell;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use implexity_core::error::{CaeError, CaeResult};
use implexity_core::json::{DumpOptions, dumps};
use serde_json::{Map, Value};

thread_local! {static SUPPRESSION_DEPTH:std::cell::Cell<usize>=const {std::cell::Cell::new(0)};}
pub struct TraceSuppression(std::marker::PhantomData<std::rc::Rc<()>>);
impl Drop for TraceSuppression {fn drop(&mut self) {SUPPRESSION_DEPTH.with(|v|v.set(v.get()-1));}}
pub fn suppress()->TraceSuppression {SUPPRESSION_DEPTH.with(|v|v.set(v.get()+1));TraceSuppression(std::marker::PhantomData)}
fn suppressed()->bool {SUPPRESSION_DEPTH.with(|v|v.get()>0)}

pub type Fields = Map<String, Value>;

pub const TRACE_ENV: &str = "IMPLEXITY_EXACT_RUNTIME_TRACE";
pub const TRACE_SCHEMA: &str = "implexity-exact-runtime-trace/1";

pub const PROTECTED_FIELDS: [&str; 6] = ["schema", "event", "time_epoch", "monotonic_ns", "pid", "thread_id"];
pub const LIFECYCLE_FIELDS: [&str; 5] = ["phase", "span_id", "duration_s", "error_type", "failure_reason"];

#[must_use]
pub fn reserved_fields() -> Vec<&'static str> {
    PROTECTED_FIELDS.iter().chain(LIFECYCLE_FIELDS.iter()).copied().collect()
}

static SEQUENCE: AtomicU64 = AtomicU64::new(1);
static THREAD_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static APPEND_LOCK: Mutex<()> = Mutex::new(());

thread_local! {
    static OBSERVERS: RefCell<Vec<Observer>> = const { RefCell::new(Vec::new()) };
    static THREAD_ID: u64 = THREAD_SEQUENCE.fetch_add(1, Ordering::Relaxed);
}

type Observer = std::rc::Rc<dyn Fn(&Value) -> CaeResult<()>>;

fn monotonic_origin() -> Instant {
    static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *ORIGIN.get_or_init(Instant::now)
}



pub fn new_trace_id(prefix: &str) -> CaeResult<String> {
    let label = prefix.trim().replace(' ', "_");
    if label.is_empty() || label.chars().any(|c| matches!(c, '/' | '\\' | '\n' | '\r' | '\t')) {
        return Err(CaeError::contract("runtime trace identifier prefix is invalid"));
    }
    Ok(format!("{label}-{}-{}", std::process::id(), SEQUENCE.fetch_add(1, Ordering::Relaxed)))
}

fn configured_path() -> CaeResult<Option<PathBuf>> {
    let Some(raw) = std::env::var_os(TRACE_ENV) else { return Ok(None) };
    let text = raw.to_string_lossy();
    if text.trim().is_empty() {
        return Ok(None);
    }
    let path = PathBuf::from(&raw);
    if !path.is_absolute() {
        return Err(CaeError::contract(format!("{TRACE_ENV} must be an absolute path")));
    }
    if path.is_dir() {
        return Ok(Some(path.join(format!("exact-runtime-{}.jsonl", std::process::id()))));
    }
    if !path.parent().is_some_and(std::path::Path::is_dir) {
        return Err(CaeError::contract(format!("{TRACE_ENV} parent directory does not exist")));
    }
    Ok(Some(path))
}

fn has_observer() -> bool {
    OBSERVERS.with(|o| !o.borrow().is_empty())
}



pub fn enabled() -> CaeResult<bool> {
    Ok(!suppressed() && (configured_path()?.is_some() || has_observer()))
}

fn encode(event: &str, fields: &Fields) -> CaeResult<String> {
    let name = event.trim();
    if name.is_empty() || name.chars().any(|c| matches!(c, '\n' | '\r' | '\t')) {
        return Err(CaeError::contract("runtime trace event name is invalid"));
    }
    let mut collision: Vec<&str> =
        PROTECTED_FIELDS.iter().copied().filter(|k| fields.contains_key(*k)).collect();
    if !collision.is_empty() {
        collision.sort_unstable();
        return Err(CaeError::contract(format!(
            "runtime trace fields collide with protected keys: {}",
            py_list(&collision)
        )));
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    let mono = u64::try_from(monotonic_origin().elapsed().as_nanos()).unwrap_or(u64::MAX);
    let mut record = Map::new();
    record.insert("schema".into(), Value::from(TRACE_SCHEMA));
    record.insert("event".into(), Value::from(name));
    record.insert("time_epoch".into(), Value::from(now));
    record.insert("monotonic_ns".into(), Value::from(mono));
    record.insert("pid".into(), Value::from(std::process::id()));
    record.insert("thread_id".into(), Value::from(THREAD_ID.with(|t| *t)));
    for (k, v) in fields {
        record.insert(k.clone(), v.clone());
    }
    let mut text = dumps(&Value::Object(record), &DumpOptions::canonical());
    text.push('\n');
    Ok(text)
}

fn py_list(keys: &[&str]) -> String {
    let inner: Vec<String> = keys.iter().map(|k| format!("'{k}'")).collect();
    format!("[{}]", inner.join(", "))
}

fn append(path: &PathBuf, encoded: &str) -> CaeResult<()> {
    let _guard = APPEND_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut options = OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|e| CaeError::contract(format!("exact runtime trace append failed: {e}")))?;
    file.write_all(encoded.as_bytes())
        .map_err(|e| CaeError::contract(format!("exact runtime trace append failed: {e}")))
}

fn notify(encoded: &str) -> CaeResult<()> {
    let observers: Vec<Observer> = OBSERVERS.with(|o| o.borrow().last().cloned().into_iter().collect());
    for observer in observers {
        let value: Value = serde_json::from_str(encoded.trim_end())
            .map_err(|e| CaeError::contract(format!("runtime trace record is not finite JSON: {e}")))?;
        observer(&value)?;
    }
    Ok(())
}

fn emit_to(path: Option<&PathBuf>, event: &str, fields: &Fields) -> CaeResult<()> {
    if suppressed() {return Ok(());}
    if path.is_none() && !has_observer() {
        return Ok(());
    }
    let encoded = encode(event, fields)?;
    if let Some(path) = path {
        append(path, &encoded)?;
    }
    notify(&encoded)
}



pub fn emit(event: &str, fields: &Fields) -> CaeResult<()> {
    emit_to(configured_path()?.as_ref(), event, fields)
}



pub fn emit_with(event: &str, fields: impl FnOnce() -> Fields) -> CaeResult<()> {
    if suppressed() {return Ok(());}
    let path = configured_path()?;
    if path.is_none() && !has_observer() {
        return Ok(());
    }
    emit_to(path.as_ref(), event, &fields())
}



pub fn point(event: &str, fields: impl FnOnce() -> Fields) -> CaeResult<()> {
    emit_with(event, || {
        let mut f = fields();
        f.insert("phase".into(), Value::from("point"));
        f
    })
}

#[must_use = "the observer is removed when the guard is dropped"]
pub struct ObserverGuard {
    _private: (),
}

impl Drop for ObserverGuard {
    fn drop(&mut self) {
        OBSERVERS.with(|o| {
            o.borrow_mut().pop();
        });
    }
}

pub fn observe(observer: impl Fn(&Value) -> CaeResult<()> + 'static) -> ObserverGuard {
    OBSERVERS.with(|o| o.borrow_mut().push(std::rc::Rc::new(observer)));
    ObserverGuard { _private: () }
}

#[must_use]
pub fn failure_fields(error: &CaeError) -> Fields {
    let mut fields = Fields::new();
    fields.insert("error_type".into(), Value::from(error.python_class()));
    if error.is_convergence() {
        let joined = error.message().split_whitespace().collect::<Vec<_>>().join(" ");
        let mut text: String = joined.chars().filter(|c| implexity_core::py_repr::is_printable(*c)).collect();
        if text.len() > 1536 {
            let mut cut = 1533;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            text.push_str("...");
        }
        if text.is_empty() {
            text = "Numerical state rejected.".into();
        }
        fields.insert("failure_reason".into(), Value::from(text));
    }
    fields
}

#[must_use]
pub fn failure_fields_of_type(type_name: &str) -> Fields {
    let mut fields = Fields::new();
    fields.insert("error_type".into(), Value::from(type_name));
    fields
}

fn validate_lifecycle(fields: &Fields) -> CaeResult<()> {
    let mut collision: Vec<&str> =
        LIFECYCLE_FIELDS.iter().copied().filter(|k| fields.contains_key(*k)).collect();
    if collision.is_empty() {
        return Ok(());
    }
    collision.sort_unstable();
    Err(CaeError::contract(format!(
        "runtime trace fields collide with lifecycle keys: {}",
        py_list(&collision)
    )))
}

pub trait TraceFailure {
    fn trace_fields(&self) -> Fields;
    fn from_trace(error: CaeError) -> Self;
}

impl TraceFailure for CaeError {
    fn trace_fields(&self) -> Fields {
        failure_fields(self)
    }
    fn from_trace(error: CaeError) -> Self {
        error
    }
}



pub fn span<T, E: TraceFailure>(
    event: &str,
    fields: impl FnOnce() -> Fields,
    body: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    let path = configured_path().map_err(E::from_trace)?;
    if path.is_none() && !has_observer() {
        return body();
    }
    let fields = fields();
    validate_lifecycle(&fields).map_err(E::from_trace)?;
    let span_id = new_trace_id("span").map_err(E::from_trace)?;
    let mut started = fields.clone();
    started.insert("phase".into(), Value::from("started"));
    started.insert("span_id".into(), Value::from(span_id.clone()));
    emit_to(path.as_ref(), event, &started).map_err(E::from_trace)?;
    let clock = Instant::now();
    match body() {
        Ok(value) => {
            let mut finished = fields;
            finished.insert("phase".into(), Value::from("finished"));
            finished.insert("span_id".into(), Value::from(span_id));
            finished.insert("duration_s".into(), Value::from(clock.elapsed().as_secs_f64()));
            emit_to(path.as_ref(), event, &finished).map_err(E::from_trace)?;
            Ok(value)
        }
        Err(error) => {
            let mut failed = fields;
            failed.insert("phase".into(), Value::from("failed"));
            failed.insert("span_id".into(), Value::from(span_id));
            failed.insert("duration_s".into(), Value::from(clock.elapsed().as_secs_f64()));
            failed.extend(error.trace_fields());
            emit_to(path.as_ref(), event, &failed).map_err(E::from_trace)?;
            Err(error)
        }
    }
}



pub fn lifecycle<T, E: TraceFailure>(
    event: &str,
    fields: impl FnOnce() -> Fields,
    callback: impl FnOnce() -> Result<T, E>,
    result_fields: impl FnOnce(&T) -> Fields,
) -> Result<T, E> {
    let path = configured_path().map_err(E::from_trace)?;
    if path.is_none() && !has_observer() {
        return callback();
    }
    let fields = fields();
    validate_lifecycle(&fields).map_err(E::from_trace)?;
    let span_id = new_trace_id("span").map_err(E::from_trace)?;
    let mut started = fields.clone();
    started.insert("phase".into(), Value::from("started"));
    started.insert("span_id".into(), Value::from(span_id.clone()));
    emit_to(path.as_ref(), event, &started).map_err(E::from_trace)?;
    let clock = Instant::now();
    let fail = |error: E, fields: Fields, span_id: String, clock: Instant| -> Result<T, E> {
        let mut failed = fields;
        failed.insert("phase".into(), Value::from("failed"));
        failed.insert("span_id".into(), Value::from(span_id));
        failed.insert("duration_s".into(), Value::from(clock.elapsed().as_secs_f64()));
        failed.extend(error.trace_fields());
        emit_to(path.as_ref(), event, &failed).map_err(E::from_trace)?;
        Err(error)
    };
    let result = match callback() {
        Ok(result) => result,
        Err(error) => return fail(error, fields, span_id, clock),
    };
    let terminal = result_fields(&result);
    let mut collision: Vec<&str> = terminal
        .keys()
        .map(String::as_str)
        .filter(|k| PROTECTED_FIELDS.contains(k) || LIFECYCLE_FIELDS.contains(k) || fields.contains_key(*k))
        .collect();
    if !collision.is_empty() {
        collision.sort_unstable();
        let error = E::from_trace(CaeError::contract(format!(
            "runtime trace lifecycle terminal fields collide with reserved keys: {}",
            py_list(&collision)
        )));
        return fail(error, fields, span_id, clock);
    }
    let mut finished = fields;
    finished.extend(terminal);
    finished.insert("phase".into(), Value::from("finished"));
    finished.insert("span_id".into(), Value::from(span_id));
    finished.insert("duration_s".into(), Value::from(clock.elapsed().as_secs_f64()));
    let encoded = encode(event, &finished).map_err(E::from_trace)?;
    if let Some(path) = path.as_ref() {
        append(path, &encoded).map_err(E::from_trace)?;
    }
    notify(&encoded).map_err(E::from_trace)?;
    Ok(result)
}

#[macro_export]
macro_rules! trace_fields {
    () => { $crate::trace::Fields::new() };
    ($($key:expr => $value:expr),+ $(,)?) => {{
        let mut fields = $crate::trace::Fields::new();
        $( fields.insert(::std::string::String::from($key), ::serde_json::json!($value)); )+
        fields
    }};
}

