// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::fmt;
use std::net::SocketAddr;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::Connected;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{ConnectInfo, FromRequestParts, State};
use axum::http::{HeaderName, HeaderValue, StatusCode, Version};
use axum::response::Response;
use axum::serve::IncomingStream;
use implexity_core::json::{DumpOptions, ParseOptions, dumps, parse_with_depth};
use serde_json::{Map, Value, json};
use tokio::net::TcpListener;

use crate::admission::{BindInfo, RequestHead, check_request};
use crate::routes::{BodyPolicy, Route, prose_list};
use crate::service::Service;

pub const MAX_REQUEST_DEPTH: usize = 128;

pub const SERVER_VERSION: &str = "implexity/1.1";

#[derive(Clone, Debug, PartialEq)]
pub enum RequestBody {
    Json(Value),
    Raw(Vec<u8>),
}

pub struct Request {
    pub service: Arc<Service>,
    pub method: String,
    pub path: String,
    pub query: String,
    pub ident: Option<String>,
    pub body: RequestBody,
    pub headers: Vec<(String, Vec<u8>)>,
    pub http11: bool,
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("path", &self.path)
            .field("ident", &self.ident)
            .finish_non_exhaustive()
    }
}

impl Request {
    #[must_use]
    pub fn json(&self) -> &Value {
        static NULL: Value = Value::Null;
        match &self.body {
            RequestBody::Json(v) => v,
            RequestBody::Raw(_) => &NULL,
        }
    }

    #[must_use]
    pub fn header(&self, name: &str) -> Option<String> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| crate::admission::latin1(v))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpReply {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
    pub headers: Vec<(String, String)>,
    pub standard_headers: bool,
    pub close: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Http(HttpReply),
    WebSocket,
}

impl Reply {
    #[must_use]
    pub fn send(status: u16, body: Vec<u8>, content_type: &str, extra: Vec<(String, String)>) -> Self {
        Self::Http(HttpReply {
            status,
            content_type: Some(content_type.to_owned()),
            body,
            headers: extra,
            standard_headers: true,
            close: false,
        })
    }

    #[must_use]
    pub fn json(value: &Value, status: u16) -> Self {
        Self::send(status, json_ascii(value).into_bytes(), "application/json", Vec::new())
    }

    #[must_use]
    #[allow(clippy::needless_pass_by_value)]
    pub fn err(status: u16, message: &str, detail: Value) -> Self {
        Self::json(&json!({"error": message, "detail": detail}), status)
    }

    #[must_use]
    pub fn reject(status: u16, message: &str, extra: Vec<(String, String)>) -> Self {
        Self::Http(reject_http(status, message, extra))
    }

    #[must_use]
    pub fn redirect(location: &str) -> Self {
        Self::Http(HttpReply {
            status: 302,
            content_type: None,
            body: Vec::new(),
            headers: vec![("Location".to_owned(), location.to_owned())],
            standard_headers: false,
            close: false,
        })
    }

    #[must_use]
    pub fn no_content() -> Self {
        Self::send(204, Vec::new(), "text/plain", Vec::new())
    }

    #[must_use]
    pub fn not_yet_ported(spec: &str, owner: &str, python: &str) -> Self {
        Self::json(
            &json!({"error": "not yet ported",
                    "detail": {"route": spec, "owner": owner, "python_handler": python}}),
            501,
        )
    }
}

fn reject_http(status: u16, message: &str, extra: Vec<(String, String)>) -> HttpReply {
    let mut headers = extra;
    headers.push(("Connection".to_owned(), "close".to_owned()));
    HttpReply {
        status,
        content_type: Some("application/json".to_owned()),
        body: json_ascii(&json!({"error": message})).into_bytes(),
        headers,
        standard_headers: true,
        close: true,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteError {
    Contract(String),
    Internal {
        message: String,
        detail: String,
    },
}

impl RouteError {
    #[must_use]
    pub fn internal(kind: &str, message: impl Into<String>) -> Self {
        let message = message.into();
        Self::Internal { detail: format!("{kind}: {message}"), message }
    }
}

impl fmt::Display for RouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Contract(m) | Self::Internal { message: m, .. } => f.write_str(m),
        }
    }
}

impl std::error::Error for RouteError {}

#[must_use]
pub fn json_ascii(value: &Value) -> String {
    dumps(value, &DumpOptions::default())
}



pub fn py_float(v: &Value) -> Result<f64, RouteError> {
    match v {
        Value::Number(n) => {
            n.as_f64().ok_or_else(|| RouteError::internal("ValueError", "number out of range"))
        }
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => {
            let t = s.trim();
            let lower = t.to_ascii_lowercase();
            match lower.trim_start_matches(['+', '-']) {
                "inf" | "infinity" | "nan" => {
                    let neg = lower.starts_with('-');
                    let base = if lower.ends_with("nan") { f64::NAN } else { f64::INFINITY };
                    Ok(if neg { -base } else { base })
                }
                _ => t.replace('_', "").parse::<f64>().map_err(|_| {
                    RouteError::internal(
                        "ValueError",
                        format!(
                            "could not convert string to float: {}",
                            implexity_core::py_repr::repr_str(s)
                        ),
                    )
                }),
            }
        }
        other => Err(RouteError::internal(
            "TypeError",
            format!("float() argument must be a string or a real number, not {}", py_type(other)),
        )),
    }
}



pub fn py_int(v: &Value) -> Result<i64, RouteError> {
    match v {
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i)
            } else {
                let f = n.as_f64().unwrap_or(f64::NAN);
                if f.is_finite() && f.abs() < 9.2e18 {
                    #[allow(clippy::cast_possible_truncation)]
                    Ok(f.trunc() as i64)
                } else {
                    Err(RouteError::internal("OverflowError", "cannot convert float to integer"))
                }
            }
        }
        Value::Bool(b) => Ok(i64::from(*b)),
        Value::String(s) => s.trim().replace('_', "").parse::<i64>().map_err(|_| {
            RouteError::internal(
                "ValueError",
                format!("invalid literal for int() with base 10: {}", implexity_core::py_repr::repr_str(s)),
            )
        }),
        other => Err(RouteError::internal(
            "TypeError",
            format!(
                "int() argument must be a string, a bytes-like object or a real number, not {}",
                py_type(other)
            ),
        )),
    }
}

#[must_use]
pub fn py_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

#[must_use]
pub fn py_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "'NoneType'",
        Value::Bool(_) => "'bool'",
        Value::Number(n) if n.is_f64() => "'float'",
        Value::Number(_) => "'int'",
        Value::String(_) => "'str'",
        Value::Array(_) => "'list'",
        Value::Object(_) => "'dict'",
    }
}



pub fn body_get<'a>(body: &'a Value, key: &str) -> Result<Option<&'a Value>, RouteError> {
    match body {
        Value::Object(m) => Ok(m.get(key)),
        other => Err(RouteError::internal(
            "AttributeError",
            format!("{} object has no attribute 'get'", py_type(other)),
        )),
    }
}

#[derive(Clone, Debug)]
pub struct BindConfig {
    pub configured_host: String,
    pub bound: SocketAddr,
}

#[derive(Clone, Copy, Debug)]
pub struct ConnInfo {
    pub remote: SocketAddr,
    pub local: SocketAddr,
}

impl Connected<IncomingStream<'_, TcpListener>> for ConnInfo {
    fn connect_info(stream: IncomingStream<'_, TcpListener>) -> Self {

        let _ = stream.io().set_nodelay(true);
        let remote = *stream.remote_addr();
        let local = stream.io().local_addr().unwrap_or(remote);
        Self { remote, local }
    }
}

#[derive(Clone)]
struct AppState {
    service: Arc<Service>,
    bind: BindConfig,
}

pub fn router(service: Arc<Service>, bind: BindConfig) -> axum::Router {
    axum::Router::new().fallback(handle).with_state(AppState { service, bind })
}




pub async fn serve<F, B>(
    service: Arc<Service>,
    host: &str,
    port: u16,
    on_bound: B,
    shutdown: F,
) -> std::io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
    B: FnOnce(SocketAddr),
{
    let bind_host = if host.is_empty() { "0.0.0.0" } else { host };
    let listener = TcpListener::bind((bind_host, port)).await?;
    let bound = listener.local_addr()?;
    service.bound(host, bound.port());
    on_bound(bound);
    let app = router(service, BindConfig { configured_host: host.to_owned(), bound });
    axum::serve(listener, app.into_make_service_with_connect_info::<ConnInfo>())
        .with_graceful_shutdown(shutdown)
        .await
}

fn log_enabled() -> bool {
    std::env::var_os("IMPLEXITY_LOG").is_some_and(|v| !v.is_empty())
}

async fn handle(
    State(app): State<AppState>,
    ConnectInfo(conn): ConnectInfo<ConnInfo>,
    request: axum::extract::Request,
) -> Response {
    let (mut parts, body) = request.into_parts();
    let method = parts.method.as_str().to_owned();
    let target = parts.uri.to_string();
    let headers: Vec<(String, Vec<u8>)> =
        parts.headers.iter().map(|(n, v)| (n.as_str().to_owned(), v.as_bytes().to_vec())).collect();
    let head = RequestHead { method: &method, target: &target, headers: &headers };
    let bind = BindInfo {
        configured_host: app.bind.configured_host.clone(),
        bound: app.bind.bound,
        local: conn.local,
    };
    let is_head = method == "HEAD";
    let reply = match check_request(&head, &bind) {
        Err(r) => Reply::reject(r.status, &r.message, r.headers),
        Ok(body_length) => {
            let ctx = Dispatch {
                service: Arc::clone(&app.service),
                method: method.clone(),
                target: target.clone(),
                headers,
                body_length,
                http11: parts.version == Version::HTTP_11,
            };
            match method.as_str() {
                "OPTIONS" => Reply::no_content(),
                "GET" | "HEAD" => ctx.dispatch("GET", false, body).await,
                "POST" => ctx.dispatch("POST", true, body).await,
                "PUT" => ctx.dispatch("PUT", false, body).await,
                other => Reply::json(&json!({"error": format!("Unsupported method ('{other}')")}), 501),
            }
        }
    };
    if log_enabled() {
        eprintln!("{} - \"{method} {target}\" {}", conn.remote, reply_status(&reply));
    }
    match reply {
        Reply::Http(r) => build_response(r, is_head),
        Reply::WebSocket => {
            if let Ok(ws) = WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
                let service = Arc::clone(&app.service);
                ws.max_message_size(crate::ws::MAX_MESSAGE_BYTES)
                    .max_frame_size(crate::ws::MAX_MESSAGE_BYTES)
                    .on_upgrade(move |socket| crate::ws::session(service, socket))
            } else {
                build_response(reject_http(400, "not a websocket upgrade", Vec::new()), is_head)
            }
        }
    }
}

fn reply_status(reply: &Reply) -> u16 {
    match reply {
        Reply::Http(r) => r.status,
        Reply::WebSocket => 101,
    }
}

struct Dispatch {
    service: Arc<Service>,
    method: String,
    target: String,
    headers: Vec<(String, Vec<u8>)>,
    body_length: u64,
    http11: bool,
}

impl Dispatch {
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| crate::admission::latin1(v))
    }

    fn no_route(&self, method: &str, path: &str) -> Reply {
        let mut reply = if method == "PUT" {
            let mut patterns = self.service.patterns_for("PUT");
            patterns.sort();
            Reply::err(405, &format!("PUT is defined for {}", prose_list(&patterns)), json!(path))
        } else {
            Reply::err(404, "no such endpoint", json!(path))
        };
        if self.body_length > 0
            && let Reply::Http(r) = &mut reply
        {
            r.close = true;
        }
        reply
    }

    async fn dispatch(self, method: &str, body_before_404: bool, body: Body) -> Reply {
        let (path, query) = self.target.split_once('?').unwrap_or((self.target.as_str(), ""));
        let (path, query) = (path.to_owned(), query.to_owned());
        let found = self.service.find_route(method, &path);
        if found.is_none() && !body_before_404 {
            return self.no_route(method, &path);
        }
        let policy = found.as_ref().map_or(BodyPolicy::Json, |m| m.route.body());
        if self.body_length > 0 {
            let content_type = self
                .header("Content-Type")
                .unwrap_or_default()
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            match policy {
                BodyPolicy::None => {
                    return Reply::reject(400, "this endpoint does not accept a body", Vec::new());
                }
                BodyPolicy::Json if content_type != "application/json" => {
                    return Reply::reject(
                        415,
                        "JSON requests require Content-Type: application/json",
                        Vec::new(),
                    );
                }
                BodyPolicy::Raw if content_type != "application/octet-stream" => {
                    return Reply::reject(
                        415,
                        "mesh uploads require Content-Type: application/octet-stream",
                        Vec::new(),
                    );
                }
                _ => {}
            }
        }
        let parsed = match self.read_body(policy, body).await {
            Ok(b) => b,
            Err(reply) => return reply,
        };
        let Some(found) = found else {
            return self.no_route(method, &path);
        };
        let request = Request {
            service: Arc::clone(&self.service),
            method: self.method.clone(),
            path,
            query,
            ident: found.ident,
            body: parsed,
            headers: self.headers,
            http11: self.http11,
        };
        call_handler(found.route, request, method).await
    }

    async fn read_body(&self, policy: BodyPolicy, body: Body) -> Result<RequestBody, Reply> {
        let n = self.body_length;
        match policy {
            BodyPolicy::None => Ok(RequestBody::Json(Value::Object(Map::new()))),
            BodyPolicy::Raw if n == 0 => {
                Err(Reply::err(400, "a mesh upload needs a Content-Length body", Value::Null))
            }
            BodyPolicy::Json if n == 0 => Ok(RequestBody::Json(Value::Object(Map::new()))),
            BodyPolicy::Raw | BodyPolicy::Json => {
                let limit = usize::try_from(n).unwrap_or(usize::MAX);
                let raw = match axum::body::to_bytes(body, limit).await {
                    Ok(b) if b.len() as u64 == n => b,
                    _ => {
                        let mut reply = Reply::err(400, "incomplete request body", Value::Null);
                        if let Reply::Http(r) = &mut reply {
                            r.close = true;
                        }
                        return Err(reply);
                    }
                };
                if policy == BodyPolicy::Raw {
                    return Ok(RequestBody::Raw(raw.to_vec()));
                }
                let text = std::str::from_utf8(&raw)
                    .map_err(|e| Reply::err(400, &format!("bad JSON body: {e}"), Value::Null))?;

                parse_with_depth(text, ParseOptions { reject_duplicate_keys: false }, MAX_REQUEST_DEPTH)
                    .map(RequestBody::Json)
                    .map_err(|e| Reply::err(400, &format!("bad JSON body: {e}"), Value::Null))
            }
        }
    }
}

fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "request handler panicked".to_owned())
}

async fn call_handler(route: Route, request: Request, method: &str) -> Reply {
    let method = method.to_owned();
    let joined =
        tokio::task::spawn_blocking(move || catch_unwind(AssertUnwindSafe(|| (route.handler())(&request))))
            .await;
    match joined {
        Ok(Ok(Ok(reply))) => reply,
        Ok(Ok(Err(RouteError::Contract(msg)))) if method == "POST" => Reply::err(400, &msg, Value::Null),
        Ok(Ok(Err(RouteError::Contract(msg)))) => {
            Reply::err(500, &msg, json!(format!("CAEContractError: {msg}")))
        }
        Ok(Ok(Err(RouteError::Internal { message, detail }))) => Reply::err(500, &message, json!(detail)),
        Ok(Err(panic)) => {
            let msg = panic_text(panic.as_ref());
            Reply::err(500, &msg, json!(format!("panic: {msg}")))
        }
        Err(join) => Reply::err(500, &join.to_string(), json!("JoinError")),
    }
}

fn build_response(reply: HttpReply, is_head: bool) -> Response {
    let status = StatusCode::from_u16(reply.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let len = reply.body.len();
    let body = if is_head { Body::empty() } else { Body::from(reply.body) };
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let h = response.headers_mut();
    let mut put = |name: &str, value: &str| {
        if let (Ok(n), Ok(v)) = (HeaderName::try_from(name), HeaderValue::try_from(value)) {
            h.append(n, v);
        }
    };
    put("Server", SERVER_VERSION);
    if reply.standard_headers {
        if let Some(ct) = &reply.content_type {
            put("Content-Type", ct);
        }
        put("Content-Length", &len.to_string());
        put("X-Content-Type-Options", "nosniff");
        let has = |k: &str| reply.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case(k));

        if !has("x-frame-options") {
            put("X-Frame-Options", "DENY");
        }
        if !has("content-security-policy") {
            put("Content-Security-Policy", "frame-ancestors 'none'");
        }
        if reply.close && !has("connection") {
            put("Connection", "close");
        }
        if !has("cache-control") {
            put("Cache-Control", "no-store");
        }
    } else {
        put("Content-Length", &len.to_string());
    }
    for (k, v) in &reply.headers {
        put(k, v);
    }
    response
}

