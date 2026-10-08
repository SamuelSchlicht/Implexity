// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use super::{
    Framework, MAX_MESSAGE, PROTOCOL, RResult, RunnerError, VERSION, append_line, canonical_hash, child_env,
    detach, dumps_line, is_monitor_poll, origin, rerr, strict_loads, terminate_process, write_json,
};
use crate::util::utc_iso;

enum Line {
    Data(Vec<u8>),
    Eof,
    Failed(String),
}

pub(crate) struct McpClient {
    output: PathBuf,
    timeout: f64,
    next_id: i64,
    pub(crate) tools: Map<String, Value>,
    inbox: Option<Receiver<Line>>,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    reader: Option<std::thread::JoinHandle<()>>,
    pub(crate) poisoned: bool,
    pub(crate) manual: Value,
}

impl McpClient {
    pub(crate) fn open(fw: &Framework, base_url: &str, output: &Path, timeout: f64) -> RResult<Self> {
        let url = origin(base_url)?.0;
        if output.exists() {
            return Err(RunnerError::Io(format!("[Errno 17] File exists: '{}'", output.display())));
        }
        std::fs::create_dir_all(output).map_err(|e| RunnerError::Io(format!("{}: {e}", output.display())))?;
        let mut client = Self {
            output: output.to_path_buf(),
            timeout,
            next_id: 1,
            tools: Map::new(),
            inbox: None,
            child: None,
            stdin: None,
            reader: None,
            poisoned: false,
            manual: Value::Null,
        };
        let stderr = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output.join("bridge_stderr.log"))?;
        let mut cmd = Command::new(&fw.bridge);
        cmd.env_clear()
            .envs(child_env(None))
            .env("IMPLEXITY_MCP_BASE_URL", &url)
            .env("IMPLEXITY_MCP_TIMEOUT_SECONDS", implexity_core::py_repr::repr_float(timeout))
            .env("IMPLEXITY_MCP_PRESERVE_ON_SIGINT", "1")
            .current_dir(&fw.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr));
        detach(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| RunnerError::Io(format!("{}: {e}", fw.bridge.display())))?;
        let stdout =
            child.stdout.take().ok_or_else(|| RunnerError::Io("bridge stdout unavailable".into()))?;
        client.stdin = child.stdin.take();
        client.child = Some(child);
        let (tx, rx) = channel();
        client.reader = Some(std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                let limit = u64::try_from(MAX_MESSAGE + 1).unwrap_or(u64::MAX);
                match (&mut reader).take(limit).read_until(b'\n', &mut line) {
                    Ok(0) => {
                        let _ = tx.send(Line::Eof);
                        return;
                    }
                    Ok(_) => {
                        if line.len() > MAX_MESSAGE || !line.ends_with(b"\n") {
                            let _ = tx.send(Line::Failed("Oversized/unframed MCP response.".into()));
                            return;
                        }
                        if tx.send(Line::Data(line)).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Line::Failed(e.to_string()));
                        return;
                    }
                }
            }
        }));
        client.inbox = Some(rx);
        let init = (|| -> RResult<()> {
            let response = client.rpc(
                "initialize",
                &json!({"protocolVersion": PROTOCOL, "capabilities": {},
                        "clientInfo": {"name": "implexity-local-study-client", "version": VERSION}}),
            )?;
            if response.get("protocolVersion").and_then(Value::as_str) != Some(PROTOCOL) {
                return rerr("Unsupported negotiated MCP version; disconnecting.");
            }
            let caps = response.get("capabilities").cloned().unwrap_or(Value::Null);
            if !(caps.get("tools").is_some() && caps.get("resources").is_some()) {
                return rerr("Server does not advertise the required MCP capabilities.");
            }
            client.notify("notifications/initialized", &json!({}))?;
            client.manual = client.rpc("resources/read", &json!({"uri": "implexity://manual"}))?;
            client.refresh_tools()?;
            Ok(())
        })();
        if let Err(e) = init {
            client.close();
            return Err(e);
        }
        Ok(client)
    }

    fn send(&mut self, value: &Value) -> RResult<()> {
        if self.poisoned {
            return Err(RunnerError::Ambiguous(
                "MCP session has an unanswered request; no mutation will be retried.".into(),
            ));
        }
        let opts = implexity_core::json::DumpOptions::compact().ascii(false);
        let mut raw = implexity_core::json::dumps(value, &opts).into_bytes();
        raw.push(b'\n');
        if raw.len() > MAX_MESSAGE {
            return rerr("Outgoing MCP message exceeds the size limit.");
        }
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| RunnerError::Ambiguous("the bridge's stdin is closed".into()))?;
        stdin
            .write_all(&raw)
            .and_then(|()| stdin.flush())
            .map_err(|e| RunnerError::Ambiguous(format!("Broken pipe: {e}")))
    }

    pub(crate) fn notify(&mut self, method: &str, params: &Value) -> RResult<()> {
        let value = json!({"jsonrpc": "2.0", "method": method, "params": params});
        self.send(&value)?;
        append_line(
            &self.output.join("notifications.jsonl"),
            &dumps_line(&json!({"direction": "sent", "utc": utc_iso(), "message": value})),
            false,
        )
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn rpc(&mut self, method: &str, params: &Value) -> RResult<Value> {
        let ident = self.next_id;
        self.next_id += 1;
        let request = json!({"jsonrpc": "2.0", "id": ident, "method": method, "params": params});
        let observation = is_monitor_poll(method, params);
        let stem = if observation { "monitor_latest".to_owned() } else { format!("{ident:06}") };
        write_json(&self.output.join(format!("{stem}_request.json")), &request, observation)?;
        let started = Instant::now();
        let mut matched = false;
        let result = (|| -> RResult<Value> {
            self.send(&request)?;
            loop {
                let remaining = self.timeout + 15.0 - started.elapsed().as_secs_f64();
                if remaining <= 0.0 {
                    return Err(RunnerError::Ambiguous("__timeout__".into()));
                }
                let inbox =
                    self.inbox.as_ref().ok_or_else(|| RunnerError::Ambiguous("__timeout__".into()))?;
                let line = match inbox.recv_timeout(super::seconds(remaining)) {
                    Ok(Line::Data(l)) => l,
                    Ok(Line::Eof) => {
                        return Err(RunnerError::Ambiguous(
                            "Bridge ended without a confirmed reply: None".into(),
                        ));
                    }
                    Ok(Line::Failed(m)) => {
                        return Err(RunnerError::Ambiguous(format!(
                            "Bridge ended without a confirmed reply: {m}"
                        )));
                    }
                    Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                        return Err(RunnerError::Ambiguous("__timeout__".into()));
                    }
                };
                let response = strict_loads(&line)?;
                if !response.is_object() || response.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
                    return rerr("Invalid MCP message envelope.");
                }
                if response.get("id").is_none() {
                    append_line(
                        &self.output.join("notifications.jsonl"),
                        &dumps_line(&json!({"direction": "received", "utc": utc_iso(), "message": response})),
                        false,
                    )?;
                    continue;
                }
                if response.get("method").is_some() {
                    let id = response["id"].clone();
                    self.send(&json!({"jsonrpc": "2.0", "id": id,
                        "error": {"code": -32601, "message": "Client capability not supported"}}))?;
                    continue;
                }
                if response["id"].as_i64() != Some(ident) || !response["id"].is_i64() {
                    return rerr("Unexpected MCP response ID; refusing to guess which action completed.");
                }
                matched = true;
                write_json(&self.output.join(format!("{stem}_response.json")), &response, observation)?;
                append_line(
                    &self.output.join("transcript.jsonl"),
                    &dumps_line(&json!({"id": ident, "method": method, "utc": utc_iso(),
                        "elapsed_s": started.elapsed().as_secs_f64(), "response_sha256": canonical_hash(&response),
                        "stored": if observation { "latest_only" } else { "full" }})),
                    true,
                )?;
                if let Some(err) = response.get("error") {
                    return rerr(format!("MCP error: {}", implexity_core::pyobj::py_str(err)));
                }
                return match response.get("result") {
                    Some(r) if r.is_object() => Ok(r.clone()),
                    _ => rerr("MCP response does not contain an object result."),
                };
            }
        })();
        match result {
            Err(RunnerError::Ambiguous(m)) if m == "__timeout__" || m.starts_with("Broken pipe") => {
                let _ = self.notify(
                    "notifications/cancelled",
                    &json!({"requestId": ident, "reason": "Local RPC timeout"}),
                );
                self.poisoned = true;
                let failure = json!({"request_id": ident, "method": method, "outcome": "unknown",
                    "reason": "No confirmed reply. Do not retry a mutating request automatically.", "utc": utc_iso()});
                let _ = write_json(&self.output.join(format!("{ident:06}_unknown.json")), &failure, false);
                Err(RunnerError::Ambiguous(
                    "No confirmed reply. Do not retry a mutating request automatically.".into(),
                ))
            }
            Err(e @ RunnerError::Ambiguous(_)) => {
                self.poisoned = true;
                Err(e)
            }
            Err(e) => {
                if !matched {
                    self.poisoned = true;
                }
                Err(e)
            }
            Ok(v) => Ok(v),
        }
    }

    pub(crate) fn refresh_tools(&mut self) -> RResult<()> {
        let mut result = self.rpc("tools/list", &json!({}))?;
        let mut all = Map::new();
        let mut cursors = std::collections::BTreeSet::new();
        loop {
            let Some(entries) = result.get("tools").and_then(Value::as_array) else {
                return rerr("Invalid tools/list response.");
            };
            for tool in entries {
                let Some(name) = tool.get("name").and_then(Value::as_str).filter(|n| !all.contains_key(*n))
                else {
                    return rerr("Invalid/repeated MCP tool name.");
                };
                all.insert(name.to_owned(), tool.clone());
            }
            let cursor = result.get("nextCursor").filter(|c| implexity_core::pyobj::truthy(c)).cloned();
            let Some(cursor) = cursor else { break };
            let Some(c) = cursor.as_str().filter(|c| !cursors.contains(*c) && cursors.len() < 100) else {
                return rerr("Invalid, repeated or excessive tools/list pagination.");
            };
            cursors.insert(c.to_owned());
            result = self.rpc("tools/list", &json!({"cursor": c}))?;
        }
        self.tools = all;
        Ok(())
    }

    pub(crate) fn action(&mut self, name: &str, payload: &Value) -> RResult<(Value, Value)> {
        self.refresh_tools()?;
        let tool = format!("implexity_{name}");
        if !self.tools.contains_key(&tool) {
            return rerr(format!("Public tool not available in current context: {tool}"));
        }
        let result = self.rpc("tools/call", &json!({"name": tool, "arguments": payload}))?;
        let mut value = result.get("structuredContent").filter(|v| v.is_object()).cloned();
        if value.is_none() {
            let texts: Vec<&str> = result
                .get("content")
                .and_then(Value::as_array)
                .map(|c| {
                    c.iter()
                        .filter(|x| x.get("type") == Some(&json!("text")))
                        .filter_map(|x| x.get("text").and_then(Value::as_str))
                        .collect()
                })
                .unwrap_or_default();
            if texts.len() == 1 {
                value = Some(strict_loads(texts[0].as_bytes())?);
            }
        }
        let ok = value.as_ref().is_some_and(|v| {
            v.is_object()
                && v.get("action").and_then(Value::as_str) == Some(name)
                && v.get("ok") == Some(&json!(true))
        }) && result.get("isError") != Some(&json!(true));
        if !ok {
            return Err(RunnerError::Action {
                action: name.to_owned(),
                response: value.filter(implexity_core::pyobj::truthy).unwrap_or_else(|| result.clone()),
            });
        }
        let payload = value.and_then(|v| v.get("result").cloned()).unwrap_or(Value::Null);
        if !payload.is_object() {
            return rerr(format!("Public action {name} returned a non-object."));
        }
        Ok((payload, result))
    }

    pub(crate) fn close(&mut self) {
        self.stdin = None;
        if let Some(mut child) = self.child.take() {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    break;
                }
                if Instant::now() >= deadline {
                    terminate_process(&mut child, Duration::from_secs(5));
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        self.inbox = None;
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        self.close();
    }
}
