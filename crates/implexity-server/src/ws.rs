// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use base64::Engine as _;
use serde_json::{Value, json};

use crate::http::{Reply, Request, RouteError, json_ascii, py_int};
use crate::preview::{PreviewContext, PreviewOp};
use crate::service::{Service, WsClient, WsFrame};

pub const MAX_MESSAGE_BYTES: usize = 8 << 20;

#[must_use]
pub fn validate_upgrade(req: &Request) -> Reply {
    let header = |n: &str| req.header(n).unwrap_or_default();
    let connection_has_upgrade =
        header("Connection").split(',').any(|p| p.trim().eq_ignore_ascii_case("upgrade"));
    if req.method != "GET"
        || !req.http11
        || !header("Upgrade").eq_ignore_ascii_case("websocket")
        || !connection_has_upgrade
    {
        return Reply::reject(400, "not a websocket upgrade", Vec::new());
    }
    if req.header("Sec-WebSocket-Version").as_deref() != Some("13") {
        return Reply::reject(
            426,
            "WebSocket version 13 is required",
            vec![("Sec-WebSocket-Version".to_owned(), "13".to_owned())],
        );
    }
    let key = header("Sec-WebSocket-Key");
    let engine = &base64::engine::general_purpose::STANDARD;
    let canonical =
        engine.decode(key.as_bytes()).ok().filter(|d| d.len() == 16).is_some_and(|d| engine.encode(d) == key);
    if !canonical {
        return Reply::reject(400, "Sec-WebSocket-Key must encode 16 bytes", Vec::new());
    }
    Reply::WebSocket
}

struct Registration {
    service: Arc<Service>,
    client: Arc<WsClient>,
    id: u64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.service.drop_client(self.id);
        self.client.close();
    }
}

pub async fn session(service: Arc<Service>, mut socket: WebSocket) {
    let client = Arc::new(WsClient::new(service.ws_queue()));
    let id = service.add_client(Arc::clone(&client));
    let _registration = Registration { service: Arc::clone(&service), client: Arc::clone(&client), id };
    let hello = json!({"event": "hello", "service": crate::http::SERVER_VERSION,
                       "backend": service.backend_name(), "design_version": service.design_version()});
    let _ = service.send_to(&client, WsFrame::Text(json_ascii(&hello)));
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    None | Some(Err(_) | Ok(Message::Close(_))) => break,
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(Value::Object(msg)) = serde_json::from_str::<Value>(text.as_str()) {
                            dispatch(&service, &client, &Value::Object(msg));
                        }
                    }

                    Some(Ok(_)) => {}
                }
            }
            () = client.notified() => {
                if !client.is_alive() {
                    break;
                }
                let mut failed = false;
                for WsFrame::Text(t) in client.drain() {
                    if socket.send(Message::text(t)).await.is_err() {
                        failed = true;
                        break;
                    }
                }
                if failed {
                    break;
                }
            }
        }
    }
}

fn off_async_workers<R>(work: impl FnOnce() -> R) -> R {
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(work)
        }
        _ => work(),
    }
}

fn reply(service: &Service, client: &WsClient, value: &Value) {
    let _ = service.send_to(client, WsFrame::Text(json_ascii(value)));
}

fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::String(s) if s.contains('\'') && !s.contains('"') => format!("\"{s}\""),
        Value::String(s) => format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'")),
        other => other.to_string(),
    }
}

fn dispatch(service: &Arc<Service>, client: &Arc<WsClient>, msg: &Value) {
    let rid = msg.get("id").cloned().unwrap_or(Value::Null);
    let kind = msg.get("op").cloned().unwrap_or(Value::Null);
    match kind.as_str() {
        Some("ping") => reply(service, client, &json!({"id": rid, "event": "pong"})),
        Some("params") => {
            let answer = match service.backend() {
                None => json!({"id": rid, "error": crate::kernel_routes::NO_BACKEND}),
                Some(b) => match off_async_workers(|| b.set_params(msg)) {
                    Ok(Value::Object(mut info)) => {
                        let version = info.get("design_version").cloned().unwrap_or(Value::Null);
                        service.broadcast(&json!({"event": "params", "design_version": version}));
                        info.insert("id".into(), rid.clone());
                        Value::Object(info)
                    }
                    Ok(other) => json!({"id": rid, "result": other}),
                    Err(e) => json!({"id": rid, "error": e.to_string()}),
                },
            };
            reply(service, client, &answer);
        }
        Some(op @ ("section" | "slab" | "probe")) => {
            let op = match op {
                "section" => PreviewOp::Section,
                "slab" => PreviewOp::Slab,
                _ => PreviewOp::Probe,
            };
            match submit(service, op, msg) {
                Ok(job) => {
                    let service = Arc::clone(service);
                    let client = Arc::clone(client);
                    tokio::task::spawn_blocking(move || {
                        if let Some(answer) = crate::preview::ws_result(&job, &rid) {
                            reply(&service, &client, &answer);
                        }
                    });
                }
                Err(e) => reply(service, client, &json!({"id": rid, "error": e.to_string()})),
            }
        }
        _ => reply(service, client, &json!({"id": rid, "error": format!("unknown op {}", py_repr(&kind))})),
    }
}



pub fn submit(
    service: &Arc<Service>,
    op: PreviewOp,
    req: &Value,
) -> Result<Arc<crate::jobs::Job<crate::preview::PreviewResult>>, RouteError> {
    let channel = match req.get("channel") {
        None => op.name().to_owned(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    };
    let seq = match req.get("seq") {
        None => 0,
        Some(v) => py_int(v)?,
    };
    let svc = Arc::clone(service);
    let body = req.clone();
    Ok(service.pool.submit(
        &channel,
        seq,
        Box::new(move |cancel| {
            let Some(backend) = svc.backend() else {
                return Err(crate::jobs::JobError::NotImplemented(
                    crate::kernel_routes::NO_BACKEND.to_owned(),
                ));
            };
            let ctx = PreviewContext { cancel, budget: &svc.budget, cache: &svc.cache };
            op.run(backend.as_ref(), &body, &ctx)
        }),
    ))
}
