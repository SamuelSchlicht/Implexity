// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::any::Any;
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Instant;

use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::http::SERVER_VERSION;
use crate::jobs::{Pool, ResultCache};
use crate::lod::{self, Budget};
use crate::preview::{PreviewBackend, PreviewResult};
use crate::routes::{Registry, RouteDeclarationError, RouteTables};
use crate::viewer::ViewerSource;

pub const DEFAULT_WS_QUEUE: usize = 256;

fn lock<M>(m: &Mutex<M>) -> MutexGuard<'_, M> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, Debug)]
pub struct ServiceConfig {
    pub workers: usize,
    pub cache_mb: u64,
    pub ws_queue: usize,
    pub workspace: Option<PathBuf>,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self { workers: 2, cache_mb: 192, ws_queue: DEFAULT_WS_QUEUE, workspace: None }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

impl ServiceConfig {


    pub fn ws_queue_from_env() -> Result<usize, ConfigError> {
        match std::env::var("IMPLEXITY_WS_QUEUE") {
            Err(_) => Ok(DEFAULT_WS_QUEUE),
            Ok(v) => v
                .trim()
                .parse::<i64>()
                .map(|n| usize::try_from(n).unwrap_or(0))
                .map_err(|_| ConfigError(format!("IMPLEXITY_WS_QUEUE={v:?} is not an integer"))),
        }
    }
}

#[must_use]
pub fn state_directory(workspace: Option<&std::path::Path>) -> PathBuf {
    if let Some(w) = workspace {
        return w.to_path_buf();
    }
    if let Some(d) = std::env::var_os("IMPLEXITY_CASE_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(".implexity").join("state")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WsFrame {
    Text(String),
}

#[derive(Debug)]
pub struct WsClient {
    queue: Mutex<VecDeque<WsFrame>>,
    capacity: usize,
    alive: AtomicBool,
    notify: Notify,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientGone;

impl WsClient {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            capacity,
            alive: AtomicBool::new(true),
            notify: Notify::new(),
        }
    }



    pub fn send(&self, frame: WsFrame) -> Result<u64, ClientGone> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err(ClientGone);
        }
        let mut q = lock(&self.queue);
        let mut dropped = 0;
        if self.capacity > 0 && q.len() >= self.capacity && q.pop_front().is_some() {
            dropped += 1;
        }
        q.push_back(frame);
        drop(q);
        self.notify.notify_one();
        Ok(dropped)
    }

    pub fn drain(&self) -> Vec<WsFrame> {
        lock(&self.queue).drain(..).collect()
    }

    pub async fn notified(&self) {
        self.notify.notified().await;
    }

    pub fn close(&self) {
        self.alive.store(false, Ordering::SeqCst);
        self.notify.notify_one();
    }

    #[must_use]
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
}

type Extension = Arc<dyn Any + Send + Sync>;

pub struct Service {
    backend: Option<Arc<dyn PreviewBackend>>,
    pub pool: Pool<PreviewResult>,
    pub cache: ResultCache<PreviewResult>,
    pub budget: Mutex<Budget>,
    routes: RwLock<RouteTables>,
    viewer: ViewerSource,
    clients: Mutex<BTreeMap<u64, Arc<WsClient>>>,
    next_client: AtomicU64,
    ws_drops: AtomicU64,
    ws_queue: usize,
    workspace: Option<PathBuf>,
    started: Instant,
    extensions: Mutex<BTreeMap<String, Extension>>,
    viewer_capture_origin: std::sync::OnceLock<String>,
    eval_lock: Mutex<Option<Arc<implexity_io::heavy_lease::HeavyOperationLease>>>,
    packages: std::sync::OnceLock<crate::packages::Packages>,
    job_lister: RwLock<Option<JobLister>>,
    geometry: std::sync::OnceLock<Arc<crate::geometry::GeometryPreview>>,
    state_lock: Arc<StateLock>,
}

#[derive(Debug, Default)]
pub struct StateLock {
    owner: Mutex<(Option<std::thread::ThreadId>, usize)>,
    freed: std::sync::Condvar,
}

#[derive(Debug)]
pub struct StateGuard(Arc<StateLock>);

impl StateLock {
    fn acquire(self: &Arc<Self>) -> StateGuard {
        let me = std::thread::current().id();
        let mut st = lock(&self.owner);
        loop {
            match st.0 {
                None => {
                    *st = (Some(me), 1);
                    break;
                }
                Some(t) if t == me => {
                    st.1 += 1;
                    break;
                }
                Some(_) => st = self.freed.wait(st).unwrap_or_else(PoisonError::into_inner),
            }
        }
        StateGuard(Arc::clone(self))
    }
}

impl Drop for StateGuard {
    fn drop(&mut self) {
        let mut st = lock(&self.0.owner);
        st.1 = st.1.saturating_sub(1);
        if st.1 == 0 {
            st.0 = None;
            self.0.freed.notify_one();
        }
    }
}

pub type JobLister = Arc<dyn Fn() -> Value + Send + Sync>;

impl fmt::Debug for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Service")
            .field("backend", &self.backend_name())
            .field("viewer", &self.viewer)
            .finish_non_exhaustive()
    }
}

impl Service {


    pub fn with_geometry(
        config: &ServiceConfig,
        kernel_routes: Registry,
        viewer: ViewerSource,
        geometry: Arc<crate::geometry::GeometryPreview>,
    ) -> std::io::Result<Self> {
        let backend: Arc<dyn PreviewBackend> = Arc::clone(&geometry) as Arc<dyn PreviewBackend>;
        let service = Self::new(config, kernel_routes, viewer, Some(backend))?;
        let _ = service.geometry.set(geometry);
        Ok(service)
    }



    pub fn new(
        config: &ServiceConfig,
        kernel_routes: Registry,
        viewer: ViewerSource,
        backend: Option<Arc<dyn PreviewBackend>>,
    ) -> std::io::Result<Self> {
        Ok(Self {
            backend,
            pool: Pool::new(config.workers)?,
            cache: ResultCache::new(config.cache_mb.saturating_mul(1024 * 1024)),
            budget: Mutex::new(Budget::default()),
            routes: RwLock::new(RouteTables::with_pending(
                kernel_routes,
                crate::kernel_routes::PENDING.iter().map(|p| format!("{} {}", p.method, p.pattern)).collect(),
            )),
            viewer,
            clients: Mutex::new(BTreeMap::new()),
            next_client: AtomicU64::new(0),
            ws_drops: AtomicU64::new(0),
            ws_queue: config.ws_queue,
            workspace: config.workspace.clone(),
            started: Instant::now(),
            extensions: Mutex::new(BTreeMap::new()),
            viewer_capture_origin: std::sync::OnceLock::new(),
            eval_lock: Mutex::new(None),
            packages: std::sync::OnceLock::new(),
            job_lister: RwLock::new(None),
            geometry: std::sync::OnceLock::new(),
            state_lock: Arc::new(StateLock::default()),
        })
    }

    #[must_use]
    pub fn backend(&self) -> Option<&Arc<dyn PreviewBackend>> {
        self.backend.as_ref()
    }

    #[must_use]
    pub fn geometry(&self) -> Option<&Arc<crate::geometry::GeometryPreview>> {
        self.geometry.get()
    }



    pub fn apply_design(
        &self,
        params: implexity_geometry::preview::Params,
        meta_updates: Option<&serde_json::Map<String, Value>>,
        source: Option<&str>,
        replace_meta: bool,
    ) -> Result<u64, crate::jobs::JobError> {
        let Some(g) = self.geometry() else {
            return Err(crate::jobs::JobError::NotImplemented(
                "design replacement needs the real geometry backend".into(),
            ));
        };
        let (version, event) = g.apply_design(params, meta_updates, source, replace_meta)?;
        self.broadcast(&event);
        Ok(version)
    }

    #[must_use]
    pub fn backend_name(&self) -> String {
        self.backend.as_ref().map_or_else(|| "none".to_owned(), |b| b.name())
    }

    #[must_use]
    pub fn design_version(&self) -> i64 {
        self.backend.as_ref().map_or(0, |b| b.design_version())
    }

    pub fn routes(&self) -> RwLockReadGuard<'_, RouteTables> {
        self.routes.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn routes_mut(&self) -> RwLockWriteGuard<'_, RouteTables> {
        self.routes.write().unwrap_or_else(PoisonError::into_inner)
    }



    pub fn register_route_table(
        &self,
        key: &str,
        table: Registry,
        owner_id: &str,
    ) -> Result<(), RouteDeclarationError> {
        self.routes_mut().register_table(key, table, owner_id)
    }



    pub fn register_gated_route_table(
        &self,
        key: &str,
        table: Registry,
        owner_id: &str,
        gate: crate::routes::RouteGate,
    ) -> Result<(), RouteDeclarationError> {
        self.routes_mut().register_gated_table(key, table, owner_id, gate)
    }



    pub fn unregister_route_table(&self, key: &str, owner_id: &str) -> Result<(), RouteDeclarationError> {
        self.routes_mut().unregister_table(key, owner_id)
    }



    pub fn eval_lock(&self) -> std::io::Result<Arc<implexity_io::heavy_lease::HeavyOperationLease>> {
        let mut slot = lock(&self.eval_lock);
        if let Some(l) = slot.as_ref() {
            return Ok(Arc::clone(l));
        }
        let dir = self.state_dir()?;
        let lease = Arc::new(
            implexity_io::heavy_lease::HeavyOperationLease::new(&dir)
                .map_err(|e| std::io::Error::other(e.0))?,
        );
        *slot = Some(Arc::clone(&lease));
        Ok(lease)
    }

    pub fn packages(self: &Arc<Self>) -> &crate::packages::Packages {
        self.packages.get_or_init(|| crate::packages::Packages::open(self))
    }

    pub fn set_job_lister(&self, lister: JobLister) {
        *self.job_lister.write().unwrap_or_else(PoisonError::into_inner) = Some(lister);
    }

    #[must_use]
    pub fn jobs_list(&self) -> Value {
        let lister = self.job_lister.read().unwrap_or_else(PoisonError::into_inner).clone();
        lister.map_or_else(|| json!({"jobs": []}), |f| f())
    }

    #[must_use]
    pub fn hold_state(&self) -> StateGuard {
        self.state_lock.acquire()
    }

    #[must_use]
    pub fn workspace(&self) -> Option<&std::path::Path> {
        self.workspace.as_deref()
    }

    #[must_use]
    pub fn find_route(&self, method: &str, path: &str) -> Option<crate::routes::RouteMatch> {
        if let Some(hit) = self.routes().find(method, path) {
            return Some(hit);
        }
        let contributions = &implexity_core::registries::global().contributions;
        for (_key, table) in implexity_core::route_tables::contributed_tables(contributions) {
            if let Some((decl, ident)) = table.matches(method, path) {

                if let Ok(route) = crate::contributed::adapt(decl) {
                    return Some(crate::routes::RouteMatch { route, ident });
                }
            }
        }
        None
    }

    #[must_use]
    pub fn patterns_for(&self, method: &str) -> Vec<String> {
        let mut out = self.routes().patterns_for(method);
        let contributions = &implexity_core::registries::global().contributions;
        for (_key, table) in implexity_core::route_tables::contributed_tables(contributions) {
            out.extend(table.all().iter().filter(|r| r.method == method).map(|r| r.pattern.clone()));
        }
        out
    }

    #[must_use]
    pub fn endpoints(&self) -> Vec<String> {
        let mut out = self.routes().endpoints();
        let contributions = &implexity_core::registries::global().contributions;
        for (_key, table) in implexity_core::route_tables::contributed_tables(contributions) {
            out.extend(table.endpoints());
        }
        out.sort();
        out
    }



    pub fn install_kernel_route(&self, route: crate::routes::Route) -> Result<(), RouteDeclarationError> {
        self.routes_mut().install_kernel_route(route)
    }



    pub fn install_kernel_table(
        &self,
        table: &implexity_core::route_tables::RouteTable,
    ) -> Result<(), RouteDeclarationError> {
        for decl in table.all() {
            self.install_kernel_route(crate::contributed::adapt(decl)?)?;
        }
        Ok(())
    }

    pub fn extension_any(
        &self,
        key: &str,
        factory: &dyn Fn() -> Arc<dyn Any + Send + Sync>,
    ) -> Arc<dyn Any + Send + Sync> {
        if let Some(hit) = lock(&self.extensions).get(key).cloned() {
            return hit;
        }
        let built = factory();
        Arc::clone(lock(&self.extensions).entry(key.to_owned()).or_insert(built))
    }



    pub fn state_dir(&self) -> std::io::Result<PathBuf> {
        let dir = state_directory(self.workspace.as_deref());
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    #[must_use]
    pub fn viewer_capture_origin(&self) -> Option<&str> {
        self.viewer_capture_origin.get().map(String::as_str)
    }

    pub fn bound(&self, host: &str, port: u16) {
        let capture_host = if matches!(host, "0.0.0.0" | "" | "localhost") { "127.0.0.1" } else { host };
        let authority = match capture_host {
            "127.0.0.1" => "127.0.0.1",
            "::1" => "[::1]",
            _ => return,
        };
        let _ = self.viewer_capture_origin.set(format!("http://{authority}:{port}"));
    }

    #[must_use]
    pub fn viewer(&self) -> &ViewerSource {
        &self.viewer
    }



    pub fn extension<T, F>(&self, key: &str, factory: F) -> Result<Arc<T>, ConfigError>
    where
        T: Any + Send + Sync,
        F: FnOnce(&Self) -> T,
    {
        let existing = lock(&self.extensions).get(key).cloned();
        let entry = if let Some(hit) = existing {
            hit
        } else {
            let built: Extension = Arc::new(factory(self));
            Arc::clone(lock(&self.extensions).entry(key.to_owned()).or_insert(built))
        };
        entry.downcast::<T>().map_err(|_| ConfigError(format!("extension {key:?} holds another type")))
    }

    pub fn add_client(&self, client: Arc<WsClient>) -> u64 {
        let id = self.next_client.fetch_add(1, Ordering::SeqCst);
        lock(&self.clients).insert(id, client);
        id
    }

    pub fn drop_client(&self, id: u64) {
        lock(&self.clients).remove(&id);
    }

    #[must_use]
    pub fn ws_queue(&self) -> usize {
        self.ws_queue
    }



    pub fn send_to(&self, client: &WsClient, frame: WsFrame) -> Result<(), ClientGone> {
        let dropped = client.send(frame)?;
        self.ws_drops.fetch_add(dropped, Ordering::SeqCst);
        Ok(())
    }

    pub fn broadcast(&self, msg: &Value) {
        let payload = crate::http::json_ascii(msg);
        let targets: Vec<(u64, Arc<WsClient>)> =
            lock(&self.clients).iter().map(|(k, v)| (*k, Arc::clone(v))).collect();
        for (id, client) in targets {
            if self.send_to(&client, WsFrame::Text(payload.clone())).is_err() {
                self.drop_client(id);
            }
        }
    }

    #[must_use]
    pub fn health(&self) -> Value {
        json!({"ok": true, "backend": self.backend_name(),
               "version": crate::COMPATIBILITY_VERSION, "rust_version": crate::RUST_VERSION})
    }

    #[must_use]
    pub fn stats(&self) -> Value {
        let uptime = lod::py_round(self.started.elapsed().as_secs_f64(), 1);
        let budget = lock(&self.budget).as_dict();
        json!({
            "service": SERVER_VERSION,
            "backend": self.backend_name(),
            "uptime_s": uptime,
            "pool": self.pool.stats().to_json(),
            "cache": self.cache.stats().to_json(),
            "budget": budget,
            "websocket_clients": lock(&self.clients).len(),
            "websocket_frames_dropped": self.ws_drops.load(Ordering::SeqCst),
            "websocket_queue": self.ws_queue,
        })
    }

    #[must_use]
    pub fn lod_info(&self) -> Value {
        json!({"levels": lod::levels_json(), "default": lod::DEFAULT, "budget": lock(&self.budget).as_dict()})
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.pool.stop();
        for client in lock(&self.clients).values() {
            client.close();
        }
    }
}

