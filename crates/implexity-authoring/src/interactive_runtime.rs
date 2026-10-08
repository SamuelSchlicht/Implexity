// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::sync::lock;

pub mod priority {
    pub const CURSOR_PREVIEW: i64 = 0;
    pub const FIELD_PREVIEW: i64 = 10;
    pub const FIELD_REFINEMENT: i64 = 20;
    pub const RESULT_EXPORT: i64 = 30;
    pub const OPTIMISATION: i64 = 0;
    pub const STUDY: i64 = 10;
}

#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    #[must_use]
    pub fn cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }


    pub fn checkpoint(&self) -> Result<(), String> {
        if self.cancelled() { Err("job cancelled".into()) } else { Ok(()) }
    }
}

struct JobState<T> {
    done: bool,
    result: Option<Result<T, String>>,
    published: bool,
}

pub struct ScheduledJob<T> {
    pub key: String,
    pub generation: u64,
    pub token: CancellationToken,
    pub lane: String,
    pub submitted_at: Instant,
    state: Arc<(Mutex<JobState<T>>, Condvar)>,
}

impl<T> Clone for ScheduledJob<T> {
    fn clone(&self) -> Self {
        Self {
            key: self.key.clone(),
            generation: self.generation,
            token: self.token.clone(),
            lane: self.lane.clone(),
            submitted_at: self.submitted_at,
            state: Arc::clone(&self.state),
        }
    }
}

impl<T: Clone> ScheduledJob<T> {

    pub fn wait(&self, timeout: Option<Duration>) -> Result<T, String> {
        let (m, cv) = &*self.state;
        let mut st = lock(m);
        let deadline = timeout.map(|t| Instant::now() + t);
        while !st.done {
            match deadline {
                None => st = cv.wait(st).unwrap_or_else(std::sync::PoisonError::into_inner),
                Some(d) => {
                    let now = Instant::now();
                    if now >= d {
                        return Err(self.key.clone());
                    }
                    st = cv.wait_timeout(st, d - now).unwrap_or_else(std::sync::PoisonError::into_inner).0;
                }
            }
        }
        st.result.clone().unwrap_or_else(|| Err("job produced no result".into()))
    }

    #[must_use]
    pub fn is_done(&self) -> bool {
        lock(&self.state.0).done
    }

    #[must_use]
    pub fn published(&self) -> bool {
        lock(&self.state.0).published
    }
}

type Work = Box<dyn FnOnce() + Send>;

struct Queued {
    priority: i64,
    sequence: u64,
    work: Work,
}

impl PartialEq for Queued {
    fn eq(&self, other: &Self) -> bool {
        (self.priority, self.sequence) == (other.priority, other.sequence)
    }
}
impl Eq for Queued {}
impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Queued {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.priority, self.sequence).cmp(&(other.priority, other.sequence))
    }
}

struct LaneShared {
    queue: Mutex<(BinaryHeap<Reverse<Queued>>, bool)>,
    cv: Condvar,
}

struct PriorityLane {
    shared: Arc<LaneShared>,
    threads: Vec<JoinHandle<()>>,
}

impl PriorityLane {
    fn new(name: &str, workers: usize) -> Self {
        let shared =
            Arc::new(LaneShared { queue: Mutex::new((BinaryHeap::new(), false)), cv: Condvar::new() });
        let threads = (0..workers.max(1))
            .filter_map(|i| {
                let s = Arc::clone(&shared);
                std::thread::Builder::new()
                    .name(format!("implexity-{name}-{i}"))
                    .spawn(move || {
                        loop {
                            let item = {
                                let mut q = lock(&s.queue);
                                while q.0.is_empty() && !q.1 {
                                    q = s.cv.wait(q).unwrap_or_else(std::sync::PoisonError::into_inner);
                                }
                                if q.1 && q.0.is_empty() {
                                    return;
                                }
                                q.0.pop()
                            };
                            if let Some(Reverse(item)) = item {
                                (item.work)();
                            }
                        }
                    })
                    .ok()
            })
            .collect();
        Self { shared, threads }
    }

    fn put(&self, item: Queued) -> Result<(), String> {
        let mut q = lock(&self.shared.queue);
        if q.1 {
            return Err("scheduler is closed".into());
        }
        q.0.push(Reverse(item));
        self.shared.cv.notify_one();
        Ok(())
    }

    fn close(&mut self, cancel: &dyn Fn()) {
        {
            let mut q = lock(&self.shared.queue);
            q.1 = true;
            cancel();
            self.shared.cv.notify_all();
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QualityProfile {
    pub name: &'static str,
    pub voxel_budget: i64,
    pub max_tile_concurrency: usize,
    pub refinement_delay_ms: u64,
    pub exact: bool,
}

#[derive(Debug)]
pub struct InteractionQualityPolicy {
    state: Mutex<(bool, f64, bool)>,
}

impl Default for InteractionQualityPolicy {
    fn default() -> Self {
        Self { state: Mutex::new((false, 16.0, false)) }
    }
}

impl InteractionQualityPolicy {
    pub fn update(&self, active: Option<bool>, frame_ms: Option<f64>, memory_pressure: Option<bool>) {
        let mut s = lock(&self.state);
        if let Some(a) = active {
            s.0 = a;
        }
        if let Some(f) = frame_ms.filter(|f| *f > 0.0) {
            s.1 = 0.85 * s.1 + 0.15 * f;
        }
        if let Some(p) = memory_pressure {
            s.2 = p;
        }
    }

    #[must_use]
    pub fn profile(&self) -> QualityProfile {
        let (active, frame_ms, pressure) = *lock(&self.state);
        if active || frame_ms > 28.0 || pressure {
            return QualityProfile {
                name: "interactive",
                voxel_budget: 48_000,
                max_tile_concurrency: 2,
                refinement_delay_ms: 220,
                exact: false,
            };
        }
        if frame_ms > 18.0 {
            return QualityProfile {
                name: "balanced",
                voxel_budget: 220_000,
                max_tile_concurrency: 3,
                refinement_delay_ms: 160,
                exact: false,
            };
        }
        QualityProfile {
            name: "exact",
            voxel_budget: i64::MAX,
            max_tile_concurrency: 5,
            refinement_delay_ms: 90,
            exact: true,
        }
    }
}

struct Slot {
    generation: u64,
    latest: Option<(CancellationToken, Arc<AtomicBool>, u64)>,
}

pub struct InteractiveRuntime {
    lanes: Mutex<HashMap<String, PriorityLane>>,
    slots: Arc<Mutex<(u64, HashMap<(String, String), Slot>)>>,
    tokens: Arc<Mutex<Vec<CancellationToken>>>,
}

impl InteractiveRuntime {
    #[must_use]
    pub fn new(interactive_workers: usize, compute_workers: usize) -> Self {
        let mut lanes = HashMap::new();
        lanes.insert("interactive".to_string(), PriorityLane::new("interactive", interactive_workers));
        lanes.insert("compute".to_string(), PriorityLane::new("compute", compute_workers));
        Self {
            lanes: Mutex::new(lanes),
            slots: Arc::new(Mutex::new((0, HashMap::new()))),
            tokens: Arc::new(Mutex::new(Vec::new())),
        }
    }


    pub fn submit<T: Clone + Send + 'static>(
        &self,
        key: &str,
        function: Box<dyn FnOnce(&CancellationToken) -> Result<T, String> + Send>,
        lane: &str,
        priority: i64,
        replace: bool,
        callback: Option<Box<dyn FnOnce(T) + Send>>,
    ) -> Result<ScheduledJob<T>, String> {
        let lanes = lock(&self.lanes);
        let Some(l) = lanes.get(lane) else { return Err(format!("unknown lane: {lane}")) };
        let slot_key = (lane.to_string(), key.to_string());
        let mut slots = lock(&self.slots);
        if let Some(Slot { latest: Some((tok, done, _)), .. }) = slots.1.get(&slot_key)
            && !done.load(Ordering::SeqCst)
        {
            if replace {
                tok.cancel();
            } else {
                return Err(format!("job already active: {key}"));
            }
        }
        slots.0 += 1;
        let sequence = slots.0;
        let slot = slots.1.entry(slot_key.clone()).or_insert(Slot { generation: 0, latest: None });
        slot.generation += 1;
        let generation = slot.generation;
        let token = CancellationToken::default();
        let done_flag = Arc::new(AtomicBool::new(false));
        slot.latest = Some((token.clone(), Arc::clone(&done_flag), generation));
        drop(slots);
        lock(&self.tokens).push(token.clone());
        let state =
            Arc::new((Mutex::new(JobState { done: false, result: None, published: false }), Condvar::new()));
        let job = ScheduledJob {
            key: key.to_string(),
            generation,
            token: token.clone(),
            lane: lane.to_string(),
            submitted_at: Instant::now(),
            state: Arc::clone(&state),
        };
        let slots_ref = Arc::clone(&self.slots);
        let work: Work = Box::new(move || {
            let finish = |r: Result<T, String>, published: bool| {
                let (m, cv) = &*state;
                let mut st = lock(m);
                st.result = Some(r);
                st.published = published;
                st.done = true;
                done_flag.store(true, Ordering::SeqCst);
                cv.notify_all();
            };
            if token.cancelled() {
                finish(Err("job cancelled".into()), false);
                return;
            }
            let outcome = function(&token).and_then(|v| token.checkpoint().map(|()| v));
            match outcome {
                Ok(value) => {
                    let mut published = false;
                    if let Some(cb) = callback {
                        let fresh = {
                            let s = lock(&slots_ref);
                            s.1.get(&slot_key)
                                .and_then(|sl| sl.latest.as_ref())
                                .is_some_and(|(_, _, g)| *g == generation)
                                && !token.cancelled()
                        };
                        if fresh {
                            cb(value.clone());
                        }
                        published = true;
                    }
                    finish(Ok(value), published);
                }
                Err(e) => finish(Err(e), false),
            }
        });
        l.put(Queued { priority, sequence, work })?;
        Ok(job)
    }


    pub fn submit_optimisation<T: Clone + Send + 'static>(
        &self,
        key: &str,
        function: Box<dyn FnOnce(&CancellationToken) -> Result<T, String> + Send>,
        callback: Option<Box<dyn FnOnce(T) + Send>>,
    ) -> Result<ScheduledJob<T>, String> {
        self.submit(key, function, "compute", priority::OPTIMISATION, false, callback)
    }

    #[must_use]
    pub fn cancel(&self, key: &str, lane: &str) -> bool {
        let slots = lock(&self.slots);
        match slots.1.get(&(lane.to_string(), key.to_string())).and_then(|s| s.latest.as_ref()) {
            Some((tok, _, _)) => {
                tok.cancel();
                true
            }
            None => false,
        }
    }

    pub fn close(&self) {
        let tokens = Arc::clone(&self.tokens);
        let cancel = move || {
            for t in lock(&tokens).iter() {
                t.cancel();
            }
        };
        for lane in lock(&self.lanes).values_mut() {
            lane.close(&cancel);
        }
    }
}

impl Drop for InteractiveRuntime {
    fn drop(&mut self) {
        self.close();
    }
}

pub struct OptimisationVisualCoalescer<F: Clone + Send + 'static> {
    runtime: Arc<InteractiveRuntime>,
    publish: Arc<dyn Fn((i64, F)) + Send + Sync>,
    min_interval: Duration,
    state: Arc<Mutex<CoalescerState<F>>>,
}

struct CoalescerState<F> {
    latest: Option<(i64, F)>,
    scheduled: bool,
    last: Option<Instant>,
}

impl<F: Clone + Send + 'static> OptimisationVisualCoalescer<F> {
    #[must_use]
    pub fn new(
        runtime: Arc<InteractiveRuntime>,
        publish: Arc<dyn Fn((i64, F)) + Send + Sync>,
        min_interval_s: f64,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            publish,
            min_interval: Duration::try_from_secs_f64(min_interval_s.max(0.0)).unwrap_or(Duration::MAX),
            state: Arc::new(Mutex::new(CoalescerState { latest: None, scheduled: false, last: None })),
        })
    }

    pub fn offer(self: &Arc<Self>, iteration: i64, field: F) {
        {
            let mut s = lock(&self.state);
            if s.latest.as_ref().is_none_or(|(i, _)| iteration >= *i) {
                s.latest = Some((iteration, field));
            }
            if s.scheduled {
                return;
            }
            s.scheduled = true;
        }
        let me = Arc::clone(self);
        let task: Box<dyn FnOnce(&CancellationToken) -> Result<(i64, F), String> + Send> =
            Box::new(move |token| {
                let last = lock(&me.state).last;
                let elapsed = last.map_or(Duration::MAX, |l| l.elapsed());
                if elapsed < me.min_interval {
                    let wait = me.min_interval.saturating_sub(elapsed);

                    let deadline = Instant::now().checked_add(wait);
                    while deadline.is_none_or(|d| Instant::now() < d) {
                        token.checkpoint()?;
                        std::thread::sleep(Duration::from_millis(10).min(
                            deadline.map_or(Duration::MAX, |d| d.saturating_duration_since(Instant::now())),
                        ));
                    }
                }
                lock(&me.state).latest.take().ok_or_else(|| "no visualisation update available".to_string())
            });
        let me2 = Arc::clone(self);
        let done: Box<dyn FnOnce((i64, F)) + Send> = Box::new(move |item| {
            (me2.publish)(item);
            let pending = {
                let mut s = lock(&me2.state);
                s.last = Some(Instant::now());
                s.scheduled = false;
                s.latest.take()
            };
            if let Some((i, f)) = pending {
                me2.offer(i, f);
            }
        });
        let _ = self.runtime.submit(
            "optimisation-visual",
            task,
            "interactive",
            priority::FIELD_PREVIEW,
            true,
            Some(done),
        );
    }
}
