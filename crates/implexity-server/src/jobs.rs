// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobError {
    Superseded(String),
    NotImplemented(String),
    Case(Vec<String>),
    Failed {
        kind: String,
        message: String,
    },
}

impl JobError {
    #[must_use]
    pub fn failed(kind: &str, message: impl Into<String>) -> Self {
        Self::Failed { kind: kind.to_owned(), message: message.into() }
    }
}

impl fmt::Display for JobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Superseded(m) | Self::NotImplemented(m) | Self::Failed { message: m, .. } => f.write_str(m),
            Self::Case(problems) => write!(f, "{problems:?}"),
        }
    }
}

impl std::error::Error for JobError {}

#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }



    pub fn check(&self) -> Result<(), JobError> {
        if self.is_cancelled() { Err(JobError::Superseded("superseded".to_owned())) } else { Ok(()) }
    }
}

pub type JobFn<T> = Box<dyn FnOnce(&CancelToken) -> Result<T, JobError> + Send>;

struct JobState<T> {
    outcome: Option<Result<T, JobError>>,
    done: bool,
    t_start: Option<Instant>,
    t_end: Option<Instant>,
}

pub struct Job<T> {
    channel: String,
    seq: i64,
    cancel: CancelToken,
    work: Mutex<Option<JobFn<T>>>,
    state: Mutex<JobState<T>>,
    done_cv: Condvar,
    t_submit: Instant,
}

impl<T> fmt::Debug for Job<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Job").field("channel", &self.channel).field("seq", &self.seq).finish_non_exhaustive()
    }
}

fn lock<M>(m: &Mutex<M>) -> MutexGuard<'_, M> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, Copy, Debug)]
pub struct JobTimes {
    pub submitted: Instant,
    pub started: Instant,
    pub ended: Instant,
}

impl<T> Job<T> {
    #[must_use]
    pub fn channel(&self) -> &str {
        &self.channel
    }

    #[must_use]
    pub fn seq(&self) -> i64 {
        self.seq
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    #[must_use]
    pub fn is_done(&self) -> bool {
        lock(&self.state).done
    }

    #[must_use]
    pub fn wait(&self, timeout: Duration) -> bool {
        let deadline = Instant::now().checked_add(timeout);
        let mut st = lock(&self.state);
        while !st.done {
            let remaining = match deadline {
                Some(d) => d.saturating_duration_since(Instant::now()),
                None => Duration::from_hours(1),
            };
            if remaining.is_zero() {
                return false;
            }
            st = self.done_cv.wait_timeout(st, remaining).unwrap_or_else(PoisonError::into_inner).0;
        }
        true
    }

    #[must_use]
    pub fn take_outcome(&self) -> Option<(Result<T, JobError>, JobTimes)> {
        let mut st = lock(&self.state);
        if !st.done {
            return None;
        }
        let times = JobTimes {
            submitted: self.t_submit,
            started: st.t_start.unwrap_or(self.t_submit),
            ended: st.t_end.unwrap_or(self.t_submit),
        };
        st.outcome.take().map(|o| (o, times))
    }

    fn finish(&self, outcome: Result<T, JobError>, started: Instant) {
        let mut st = lock(&self.state);
        st.outcome = Some(outcome);
        st.t_start = Some(started);
        st.t_end = Some(Instant::now());
        st.done = true;
        drop(st);
        self.done_cv.notify_all();
    }
}

struct PoolInner<T> {
    queue: Vec<Arc<Job<T>>>,
    live: BTreeMap<String, Vec<Arc<Job<T>>>>,
    stop: bool,
    n_superseded: u64,
    n_done: u64,
}

struct PoolShared<T> {
    inner: Mutex<PoolInner<T>>,
    cv: Condvar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolStats {
    pub queued: usize,
    pub workers: usize,
    pub superseded: u64,
    pub completed: u64,
}

impl PoolStats {
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({"queued": self.queued, "workers": self.workers,
               "superseded": self.superseded, "completed": self.completed})
    }
}

pub struct Pool<T: Send + 'static> {
    shared: Arc<PoolShared<T>>,
    threads: Vec<JoinHandle<()>>,
}

impl<T: Send + 'static> fmt::Debug for Pool<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pool").field("workers", &self.threads.len()).finish_non_exhaustive()
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "worker panicked".to_owned())
}

impl<T: Send + 'static> Pool<T> {


    pub fn new(workers: usize) -> std::io::Result<Self> {
        let shared = Arc::new(PoolShared {
            inner: Mutex::new(PoolInner {
                queue: Vec::new(),
                live: BTreeMap::new(),
                stop: false,
                n_superseded: 0,
                n_done: 0,
            }),
            cv: Condvar::new(),
        });
        let mut threads = Vec::new();
        for i in 0..workers.max(1) {
            let s = Arc::clone(&shared);
            threads.push(
                std::thread::Builder::new().name(format!("implexity-worker-{i}")).spawn(move || run(&s))?,
            );
        }
        Ok(Self { shared, threads })
    }

    #[must_use = "the job handle is how the result is collected"]
    pub fn submit(&self, channel: &str, seq: i64, work: JobFn<T>) -> Arc<Job<T>> {
        let job = Arc::new(Job {
            channel: channel.to_owned(),
            seq,
            cancel: CancelToken::new(),
            work: Mutex::new(Some(work)),
            state: Mutex::new(JobState { outcome: None, done: false, t_start: None, t_end: None }),
            done_cv: Condvar::new(),
            t_submit: Instant::now(),
        });
        let mut inner = lock(&self.shared.inner);
        let mut superseded = 0;
        let live = inner.live.entry(channel.to_owned()).or_default();
        for other in live.iter() {
            if other.seq < seq && !other.is_done() {
                other.cancel();
                superseded += 1;
            }
        }
        live.push(Arc::clone(&job));
        live.retain(|j| !j.is_done());
        inner.n_superseded += superseded;
        inner.queue.push(Arc::clone(&job));
        drop(inner);
        self.shared.cv.notify_one();
        job
    }

    pub fn stop(&self) {
        lock(&self.shared.inner).stop = true;
        self.shared.cv.notify_all();
    }

    #[must_use]
    pub fn stats(&self) -> PoolStats {
        let inner = lock(&self.shared.inner);
        PoolStats {
            queued: inner.queue.len(),
            workers: self.threads.len(),
            superseded: inner.n_superseded,
            completed: inner.n_done,
        }
    }
}

impl<T: Send + 'static> Drop for Pool<T> {
    fn drop(&mut self) {
        self.stop();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn run<T: Send + 'static>(shared: &PoolShared<T>) {
    loop {
        let job = {
            let mut inner = lock(&shared.inner);
            while inner.queue.is_empty() && !inner.stop {
                inner = shared
                    .cv
                    .wait_timeout(inner, Duration::from_millis(500))
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
            if inner.stop {
                return;
            }
            match inner.queue.pop() {
                Some(j) => j,
                None => continue,
            }
        };
        let started = Instant::now();
        if job.is_cancelled() {
            job.finish(Err(JobError::Superseded("cancelled before start".to_owned())), started);
            continue;
        }
        let work = lock(&job.work).take();
        let outcome = match work {
            Some(f) => catch_unwind(AssertUnwindSafe(|| f(&job.cancel)))
                .unwrap_or_else(|p| Err(JobError::failed("panic", panic_message(p.as_ref())))),
            None => Err(JobError::failed("RuntimeError", "job ran twice")),
        };
        job.finish(outcome, started);
        lock(&shared.inner).n_done += 1;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheStats {
    pub entries: usize,
    pub bytes: u64,
    pub max_bytes: u64,
    pub hits: u64,
    pub misses: u64,
}

impl CacheStats {
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({"entries": self.entries, "bytes": self.bytes, "max_bytes": self.max_bytes,
               "hits": self.hits, "misses": self.misses})
    }
}

struct CacheInner<V> {
    entries: BTreeMap<String, (V, u64, u64)>,
    order: BTreeMap<u64, String>,
    tick: u64,
    bytes: u64,
    hits: u64,
    misses: u64,
}

pub struct ResultCache<V: Clone> {
    max_bytes: u64,
    inner: Mutex<CacheInner<V>>,
}

impl<V: Clone> fmt::Debug for ResultCache<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResultCache").field("max_bytes", &self.max_bytes).finish_non_exhaustive()
    }
}

impl<V: Clone> ResultCache<V> {
    #[must_use]
    pub fn new(max_bytes: u64) -> Self {
        Self {
            max_bytes,
            inner: Mutex::new(CacheInner {
                entries: BTreeMap::new(),
                order: BTreeMap::new(),
                tick: 0,
                bytes: 0,
                hits: 0,
                misses: 0,
            }),
        }
    }

    #[must_use]
    pub fn key<S: AsRef<str>>(parts: &[S]) -> String {
        let mut h = Sha256::new();
        for p in parts {
            h.update(p.as_ref().as_bytes());
            h.update(b"\x1f");
        }
        hex::encode(&h.finalize()[..16])
    }

    pub fn get(&self, k: &str) -> Option<V> {
        let mut inner = lock(&self.inner);
        let inner = &mut *inner;
        let Some((value, _, tick)) = inner.entries.get_mut(k) else {
            inner.misses += 1;
            return None;
        };
        inner.order.remove(tick);
        inner.tick += 1;
        *tick = inner.tick;
        inner.order.insert(inner.tick, k.to_owned());
        inner.hits += 1;
        Some(value.clone())
    }

    pub fn put(&self, k: &str, value: V, nbytes: u64) {
        let mut inner = lock(&self.inner);
        let inner = &mut *inner;
        if let Some((_, old, tick)) = inner.entries.remove(k) {
            inner.bytes -= old;
            inner.order.remove(&tick);
        }
        inner.tick += 1;
        inner.entries.insert(k.to_owned(), (value, nbytes, inner.tick));
        inner.order.insert(inner.tick, k.to_owned());
        inner.bytes += nbytes;
        while inner.bytes > self.max_bytes {
            let Some((_, oldest)) = inner.order.pop_first() else { break };
            if let Some((_, nb, _)) = inner.entries.remove(&oldest) {
                inner.bytes -= nb;
            }
        }
    }

    #[must_use]
    pub fn stats(&self) -> CacheStats {
        let inner = lock(&self.inner);
        CacheStats {
            entries: inner.entries.len(),
            bytes: inner.bytes,
            max_bytes: self.max_bytes,
            hits: inner.hits,
            misses: inner.misses,
        }
    }
}

