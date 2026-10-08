// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::io::{BufRead as _, BufReader, Write as _};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{RResult, RunnerError, framework_root, rerr};

fn pretty(v: &Value) -> String {
    implexity_core::json::dumps(v, &implexity_core::json::DumpOptions::indented(2))
}

fn write(path: &Path, text: &str) -> RResult<()> {
    std::fs::write(path, text).map_err(|e| RunnerError::Io(format!("{}: {e}", path.display())))
}

#[allow(clippy::too_many_lines)]
pub(crate) fn replay(
    actions: &Value,
    base_url: &str,
    output: &Path,
    timeout: f64,
    framework: Option<&str>,
) -> RResult<Value> {
    let rows = actions.as_array().filter(|rows| {
        rows.iter().all(|r| {
            r.as_object().is_some_and(|m| {
                m.len() == 2
                    && m.get("action").is_some_and(Value::is_string)
                    && m.get("payload").is_some_and(Value::is_object)
            })
        })
    });
    let Some(rows) = rows.cloned() else {
        return Err(RunnerError::Runner("actions must be explicit action/payload objects".into()));
    };
    let fw = framework_root(framework)?;
    if output.exists() {
        return Err(RunnerError::Io(format!("[Errno 17] File exists: '{}'", output.display())));
    }
    std::fs::create_dir_all(output)?;
    write(&output.join("actions.json"), &pretty(&Value::Array(rows.clone())))?;
    let err = std::fs::File::create(output.join("bridge_stderr.log"))?;
    let mut child = Command::new(&fw.bridge)
        .env("IMPLEXITY_MCP_BASE_URL", base_url)
        .env("IMPLEXITY_MCP_TIMEOUT_SECONDS", implexity_core::py_repr::repr_float(timeout))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(err))
        .spawn()
        .map_err(|e| RunnerError::Io(format!("{}: {e}", fw.bridge.display())))?;
    let stdout = child.stdout.take().ok_or_else(|| RunnerError::Io("bridge stdout unavailable".into()))?;
    let mut stdin = child.stdin.take().ok_or_else(|| RunnerError::Io("bridge stdin unavailable".into()))?;
    let (tx, rx) = channel::<Option<String>>();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(l) => {
                    if tx.send(Some(l)).is_err() {
                        return;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = tx.send(None);
    });
    let mut records: Vec<Value> = Vec::new();
    let mut last: Option<Value> = None;
    let send = |stdin: &mut std::process::ChildStdin, v: &Value| -> RResult<()> {
        let line = implexity_core::json::dumps(v, &implexity_core::json::DumpOptions::default());
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.write_all(b"\n"))
            .and_then(|()| stdin.flush())?;
        Ok(())
    };
    let mut body = || -> RResult<Value> {
        let call = |stdin: &mut std::process::ChildStdin,
                    method: &str,
                    params: Value,
                    records: &mut Vec<Value>|
         -> RResult<Value> {
            let ident = records.len() + 1;
            let request = json!({"jsonrpc": "2.0", "id": ident, "method": method, "params": params});
            let started = Instant::now();
            send(stdin, &request)?;
            write(&output.join(format!("{ident:03}_request.json")), &pretty(&request))?;
            let outcome = (|| -> RResult<Value> {
                let line = match rx.recv_timeout(super::seconds(timeout + 10.0)) {
                    Ok(Some(l)) => l,
                    Ok(None) => return rerr("MCP bridge closed stdout"),
                    Err(_) => return rerr("timed out"),
                };
                let response: Value = serde_json::from_str(&line)
                    .map_err(|e| RunnerError::Runner(format!("invalid JSON from the bridge: {e}")))?;
                records.push(json!({"request": request, "response": response, "elapsed_s": started.elapsed().as_secs_f64()}));
                write(&output.join(format!("{ident:03}_response.json")), &pretty(&response))?;
                write(&output.join("transcript.json"), &pretty(&Value::Array(records.clone())))?;
                if response.get("id").and_then(Value::as_u64) != u64::try_from(ident).ok()
                    || response.get("error").is_some()
                {
                    return rerr(implexity_core::pyobj::py_str(&response));
                }
                let result = response.get("result").cloned().unwrap_or(Value::Null);
                if result.get("isError").is_some_and(implexity_core::pyobj::truthy) {
                    return rerr(implexity_core::pyobj::py_str(
                        result.get("structuredContent").unwrap_or(&result),
                    ));
                }
                Ok(result)
            })();
            if let Err(e) = &outcome {
                let failure = json!({"request": request, "error": e.to_string(), "elapsed_s": started.elapsed().as_secs_f64()});
                write(&output.join("failure.json"), &pretty(&failure))?;
            }
            outcome
        };
        call(
            &mut stdin,
            "initialize",
            json!({"protocolVersion": super::PROTOCOL, "capabilities": {},
                   "clientInfo": {"name": "implexity-explicit-action-replay", "version": "1"}}),
            &mut records,
        )?;
        send(&mut stdin, &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        call(&mut stdin, "resources/read", json!({"uri": "implexity://manual"}), &mut records)?;
        for row in &rows {
            call(&mut stdin, "tools/list", json!({}), &mut records)?;
            let action = row["action"].as_str().unwrap_or_default();
            let result = call(
                &mut stdin,
                "tools/call",
                json!({"name": format!("implexity_{action}"), "arguments": row["payload"]}),
                &mut records,
            )?;
            let structured = result.get("structuredContent").cloned().unwrap_or_else(|| json!({}));
            if structured.get("ok") != Some(&json!(true)) {
                return rerr(implexity_core::pyobj::py_str(&structured));
            }
            let summary = structured.get("result").cloned().unwrap_or_else(|| json!({}));
            let mut shown = serde_json::Map::new();
            for k in ["id", "job_id", "status", "content_id", "schema"] {
                if let Some(v) = summary.get(k) {
                    shown.insert(k.to_owned(), v.clone());
                }
            }
            println!(
                "{action} {}",
                implexity_core::json::dumps(
                    &Value::Object(shown),
                    &implexity_core::json::DumpOptions::default()
                )
            );
            last = Some(structured);
        }
        let final_ = json!({"status": "completed", "actions": rows.len(), "rpc_calls": records.len(),
            "transport": "MCP stdio JSON-RPC to existing loopback agent",
            "last_result": if rows.is_empty() { Value::Null } else { last.clone().unwrap_or(Value::Null) }});
        write(&output.join("result.json"), &pretty(&final_))?;
        Ok(final_)
    };
    let result = body();
    drop(stdin);
    if matches!(child.try_wait(), Ok(None)) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) {
            std::thread::sleep(Duration::from_millis(20));
        }
        if matches!(child.try_wait(), Ok(None)) {
            super::terminate_process(&mut child, Duration::from_secs(5));
        }
    }
    let _ = reader.join();
    result
}

pub(crate) fn main(
    actions: &str,
    output: &str,
    base_url: &str,
    timeout: f64,
    framework: Option<&str>,
) -> RResult<u8> {
    let text = std::fs::read_to_string(actions).map_err(|e| RunnerError::Io(format!("{actions}: {e}")))?;
    let value: Value =
        serde_json::from_str(&text).map_err(|e| RunnerError::Runner(format!("{actions}: {e}")))?;
    replay(&value, base_url, &crate::util::expand_abs(output), timeout, framework)?;
    Ok(0)
}
