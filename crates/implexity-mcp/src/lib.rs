// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt;
use std::io::{BufRead, Write};
use std::net::IpAddr;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use implexity_core::json::{DumpOptions, ParseOptions, dumps, parse_with};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

pub const CRATE: &str = "implexity-mcp";

pub const LAYER: &str = "kernel";

pub const PROTOCOL_VERSION: &str = "2025-11-25";
pub const SERVER_NAME: &str = "implexity";
pub const SERVER_VERSION: &str = "24.0.0rc12.dev9";
pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8765";
pub const TOOLS_PATH: &str = "/v1/agent/tools";
pub const ACTION_PATH: &str = "/v1/agent/action";
pub const MANUAL_PATH: &str = "/v1/agent/manual";
pub const MANUAL_URI: &str = "implexity://manual";
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_IMAGE_PIXELS: i64 = 4 * 1024 * 1024;
pub const DEFAULT_TIMEOUT_SECONDS: f64 = 120.0;
pub const MAX_TIMEOUT_SECONDS: f64 = 14_700.0;
pub const INLINE_FILE_SCHEMA: &str = "implexity-inline-file/1";
pub const RUST_EXTENSION: &str = "implexity-rust-extension/1";

#[derive(Clone, Debug, PartialEq)]
pub enum BridgeError {
    Bridge(String),
    InvalidParams(String),
    ActionHttp {
        status: u16,
        body: Map<String, Value>,
    },
}

impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bridge(m) | Self::InvalidParams(m) => f.write_str(m),
            Self::ActionHttp { status, .. } => write!(f, "Implexity action returned HTTP {status}"),
        }
    }
}

impl std::error::Error for BridgeError {}

fn bridge(msg: impl Into<String>) -> BridgeError {
    BridgeError::Bridge(msg.into())
}



pub fn strict_json(raw: &[u8], label: &str) -> Result<Value, BridgeError> {
    let text =
        std::str::from_utf8(raw).map_err(|e| bridge(format!("{label} is not strict UTF-8 JSON: {e}")))?;
    parse_with(text, ParseOptions { reject_duplicate_keys: false }).map_err(|e| {
        bridge(format!(
            "{label} is not strict UTF-8 JSON: {}: line {} column {} (char {})",
            e.message, e.line, e.column, e.offset
        ))
    })
}

#[must_use]
pub fn compact_json(value: &Value) -> String {
    dumps(value, &DumpOptions::canonical().ascii(false))
}

#[derive(Debug, Default)]
struct Split {
    scheme: String,
    netloc: String,
    path: String,
    query: String,
    fragment: String,
}

fn urlsplit(url: &str) -> Result<Split, String> {
    let mut rest = url;
    let mut s = Split::default();
    if let Some(i) = rest.find(':')
        && i > 0
        && rest.starts_with(|c: char| c.is_ascii_alphabetic())
        && rest[..i].chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
    {
        s.scheme = rest[..i].to_ascii_lowercase();
        rest = &rest[i + 1..];
    }
    if let Some(after) = rest.strip_prefix("//") {
        let delim = after.find(['/', '?', '#']).unwrap_or(after.len());
        after[..delim].clone_into(&mut s.netloc);
        rest = &after[delim..];
        if s.netloc.contains('[') != s.netloc.contains(']') {
            return Err("Invalid IPv6 URL".to_owned());
        }
    }
    let (rest, fragment) = rest.split_once('#').unwrap_or((rest, ""));
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    fragment.clone_into(&mut s.fragment);
    path.clone_into(&mut s.path);
    query.clone_into(&mut s.query);
    Ok(s)
}

impl Split {
    fn userinfo(&self) -> (String, String) {
        match self.netloc.rsplit_once('@') {
            Some((info, _)) => {
                let (u, p) = info.split_once(':').unwrap_or((info, ""));
                (u.to_owned(), p.to_owned())
            }
            None => (String::new(), String::new()),
        }
    }

    fn hostinfo(&self) -> (&str, &str) {
        let hostinfo = self.netloc.rsplit_once('@').map_or(self.netloc.as_str(), |(_, h)| h);
        if let Some((_, bracketed)) = hostinfo.split_once('[') {
            let (host, after) = bracketed.split_once(']').unwrap_or((bracketed, ""));
            (host, after.split_once(':').map_or("", |(_, p)| p))
        } else {
            hostinfo.split_once(':').unwrap_or((hostinfo, ""))
        }
    }

    fn port(&self) -> Result<Option<u32>, String> {
        let (_, port) = self.hostinfo();
        if port.is_empty() {
            return Ok(None);
        }
        if !port.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("Port could not be cast to integer value as '{port}'"));
        }
        let trimmed = port.trim_start_matches('0');
        let value: u32 = if trimmed.is_empty() {
            0
        } else if trimmed.len() > 5 {
            u32::MAX
        } else {
            trimmed.parse().unwrap_or(u32::MAX)
        };
        if value > 65535 {
            return Err("Port out of range 0-65535".to_owned());
        }
        Ok(Some(value))
    }
}



pub fn base_url(raw: Option<&str>) -> Result<String, BridgeError> {
    let raw = raw.unwrap_or(DEFAULT_BASE_URL).trim();
    let parsed = urlsplit(raw).map_err(|e| bridge(format!("invalid MCP base URL: {e}")))?;
    let port = parsed.port().map_err(|e| bridge(format!("invalid MCP base URL: {e}")))?;
    let (user, password) = parsed.userinfo();
    if parsed.scheme != "http" || !user.is_empty() || !password.is_empty() {
        return Err(bridge("MCP base URL must be loopback HTTP without credentials"));
    }
    if !(parsed.path.is_empty() || parsed.path == "/")
        || !parsed.query.is_empty()
        || !parsed.fragment.is_empty()
    {
        return Err(bridge("MCP base URL must be an origin"));
    }
    let host = parsed.hostinfo().0.to_lowercase();
    let address: IpAddr = if host.contains(':') {
        host.parse::<std::net::Ipv6Addr>().map(IpAddr::V6).ok()
    } else {
        host.parse::<std::net::Ipv4Addr>().map(IpAddr::V4).ok()
    }
    .ok_or_else(|| bridge("MCP base URL must use a literal loopback address"))?;
    if !address.is_loopback() {
        return Err(bridge("MCP bridge refuses non-loopback addresses"));
    }
    let effective_port = port.unwrap_or(80);
    if !(1..=65535).contains(&effective_port) {
        return Err(bridge("MCP port must be between 1 and 65535"));
    }
    Ok(match address {
        IpAddr::V6(v6) => format!("http://[{v6}]:{effective_port}"),
        IpAddr::V4(v4) => format!("http://{v4}:{effective_port}"),
    })
}



pub fn timeout_seconds(raw: Option<&str>) -> Result<f64, BridgeError> {
    let Some(raw) = raw else { return Ok(DEFAULT_TIMEOUT_SECONDS) };
    let value: f64 =
        raw.trim().replace('_', "").parse().map_err(|_| bridge("MCP timeout must be numeric"))?;
    if !(1.0..=MAX_TIMEOUT_SECONDS).contains(&value) {
        return Err(bridge("MCP timeout is outside the supported range"));
    }
    Ok(value)
}

pub trait Transport {


    fn exchange(&self, method: &str, url: &str, body: Option<Vec<u8>>)
    -> Result<(u16, Vec<u8>), BridgeError>;
}

#[derive(Debug)]
struct LiteralLoopbackResolver;

impl ureq::unversioned::resolver::Resolver for LiteralLoopbackResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        _config: &ureq::config::Config,
        _timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        let authority = uri.authority().ok_or(ureq::Error::HostNotFound)?;
        let host = authority.host();
        let host = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
        let ip: IpAddr = host.parse().map_err(|_| ureq::Error::HostNotFound)?;
        let port = authority.port_u16().ok_or(ureq::Error::HostNotFound)?;
        if !ip.is_loopback() {
            return Err(ureq::Error::HostNotFound);
        }
        let mut addrs = self.empty();
        addrs.push(std::net::SocketAddr::new(ip, port));
        Ok(addrs)
    }
}


#[derive(Debug)]
pub struct HttpTransport {
    agent: ureq::Agent,
}

impl HttpTransport {
    #[must_use]
    pub fn new(timeout_s: f64) -> Self {
        let t = Some(Duration::from_secs_f64(timeout_s));
        let config = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .max_idle_connections(0)
            .max_idle_connections_per_host(0)
            .timeout_global(t)
            .timeout_connect(t)
            .timeout_send_request(t)
            .timeout_send_body(t)
            .timeout_recv_response(t)
            .timeout_recv_body(t)
            .build();
        Self {
            agent: ureq::Agent::with_parts(
                config,
                ureq::unversioned::transport::DefaultConnector::new(),
                LiteralLoopbackResolver,
            ),
        }
    }
}

impl Transport for HttpTransport {
    fn exchange(
        &self,
        method: &str,
        url: &str,
        body: Option<Vec<u8>>,
    ) -> Result<(u16, Vec<u8>), BridgeError> {
        let unavailable = |e: ureq::Error| bridge(format!("Implexity public service is unavailable: {e}"));
        let response = match (method, body) {
            ("POST", Some(b)) => self
                .agent
                .post(url)
                .header("Accept", "application/json")
                .header("Content-Type", "application/json")
                .header("Connection", "close")
                .send(&b[..]),
            ("GET", None) => {
                self.agent.get(url).header("Accept", "application/json").header("Connection", "close").call()
            }
            _ => return Err(bridge("request is outside the public endpoint allow-list")),
        };
        let mut response = response.map_err(unavailable)?;
        let status = response.status().as_u16();
        let raw =
            response.body_mut().with_config().limit(MAX_RESPONSE_BYTES as u64 + 1).read_to_vec().map_err(
                |e| match e {
                    ureq::Error::BodyExceedsLimit(_) => bridge("Implexity response exceeded 64 MiB"),
                    other => unavailable(other),
                },
            )?;
        if raw.len() > MAX_RESPONSE_BYTES {
            return Err(bridge("Implexity response exceeded 64 MiB"));
        }
        Ok((status, raw))
    }
}

fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Number(n) => implexity_core::json::number_text(n),
        other => compact_json(other),
    }
}

fn py_title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_cased = false;
    for c in s.chars() {
        if prev_cased {
            out.extend(c.to_lowercase());
        } else {
            out.extend(c.to_uppercase());
        }
        prev_cased = c.is_alphabetic();
    }
    out
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null | Value::Bool(false)) => false,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Bool(true)) => true,
    }
}

pub struct PublicAgentClient<T: Transport> {
    base_url: String,
    transport: T,
    tools: Option<BTreeMap<String, Map<String, Value>>>,
}

impl<T: Transport> fmt::Debug for PublicAgentClient<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PublicAgentClient").field("base_url", &self.base_url).finish_non_exhaustive()
    }
}

impl<T: Transport> PublicAgentClient<T> {
    pub fn new(base_url: String, transport: T) -> Self {
        Self { base_url, transport, tools: None }
    }



    pub fn request(
        &self,
        method: &str,
        path: &str,
        payload: Option<&Value>,
    ) -> Result<Map<String, Value>, BridgeError> {
        let allowed = matches!((method, path), ("GET", TOOLS_PATH | MANUAL_PATH) | ("POST", ACTION_PATH));
        if !allowed {
            return Err(bridge("request is outside the public endpoint allow-list"));
        }
        let body = payload.map(|p| compact_json(p).into_bytes());
        let url = format!("{}{path}", self.base_url);
        let (status, raw) = self.transport.exchange(method, &url, body)?;
        if !(200..300).contains(&status) {
            let parsed = if raw.is_empty() {
                Value::Object(Map::new())
            } else {
                strict_json(&raw, "Implexity error response")?
            };
            if method == "POST"
                && let Value::Object(body) = parsed
            {
                return Err(BridgeError::ActionHttp { status, body });
            }
            return Err(bridge(format!("Implexity returned HTTP {status}")));
        }
        match strict_json(&raw, "Implexity response")? {
            Value::Object(m) => Ok(m),
            _ => Err(bridge("Implexity response must be an object")),
        }
    }



    pub fn list_tools(&mut self) -> Result<Vec<Value>, BridgeError> {
        let manifest = self.request("GET", TOOLS_PATH, None)?;
        if manifest.get("schema") != Some(&json!("implexity-agent-tool-manifest/1")) {
            return Err(bridge("unexpected Implexity tool-manifest schema"));
        }
        let Some(Value::Array(entries)) = manifest.get("tools") else {
            return Err(bridge("tool manifest lacks a tools array"));
        };
        let mut live = BTreeMap::new();
        let mut result = Vec::new();
        for entry in entries {
            let Value::Object(entry) = entry else {
                return Err(bridge("tool manifest contains a non-object"));
            };
            let action = entry.get("action").unwrap_or(&Value::Null);
            let schema = entry.get("input_schema");
            let valid = match (entry.get("name"), schema) {
                (Some(Value::String(name)), Some(Value::Object(s))) => {
                    *name == format!("implexity_{}", py_str(action))
                        && s.get("type") == Some(&json!("object"))
                }
                _ => false,
            };
            if !valid {
                return Err(bridge("tool manifest contains an invalid binding"));
            }
            let name = entry.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
            if live.contains_key(&name) {
                return Err(bridge(format!("tool manifest repeats {name}")));
            }
            if entry.get("allowed") == Some(&Value::Bool(true)) {
                let Value::String(action_name) = action else {
                    return Err(bridge("unexpected Implexity MCP failure"));
                };
                let description = if truthy(entry.get("description")) {
                    py_str(entry.get("description").unwrap_or(&Value::Null))
                } else {
                    action_name.clone()
                };
                result.push(json!({
                    "name": name,
                    "title": py_title(&action_name.replace('_', " ")),
                    "description": description,
                    "inputSchema": schema.cloned().unwrap_or(Value::Null),
                    "annotations": {"openWorldHint": false},
                }));
            }
            live.insert(name, entry.clone());
        }
        self.tools = Some(live);
        Ok(result)
    }



    pub fn read_manual(&self) -> Result<String, BridgeError> {
        let manual = self.request("GET", MANUAL_PATH, None)?;
        if manual.get("schema") != Some(&json!("implexity-engineering-agent-manual/1")) {
            return Err(bridge("unexpected Implexity manual schema"));
        }
        Ok(compact_json(&Value::Object(manual)))
    }



    pub fn call_tool(
        &mut self,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Map<String, Value>, BridgeError> {
        if self.tools.is_none() {
            self.list_tools()?;
        }
        let entry = self.tools.as_ref().and_then(|t| t.get(name)).cloned();
        let Some(entry) = entry.filter(|e| e.get("allowed") == Some(&Value::Bool(true))) else {
            return Err(BridgeError::InvalidParams(format!("unknown or unavailable Implexity tool: {name}")));
        };
        let action = entry.get("action").cloned().unwrap_or(Value::Null);
        let response = self.request(
            "POST",
            ACTION_PATH,
            Some(&json!({"action": action, "payload": Value::Object(arguments.clone())})),
        )?;
        if response.get("action") != Some(&action) || !matches!(response.get("ok"), Some(Value::Bool(_))) {
            return Err(bridge("action response violates the public contract"));
        }
        Ok(response)
    }
}

#[must_use]
pub fn rpc_result(id: &Value, result: Value) -> Value {
    let mut m = Map::new();
    m.insert("jsonrpc".into(), json!("2.0"));
    m.insert("id".into(), id.clone());
    m.insert("result".into(), result);
    Value::Object(m)
}

#[must_use]
pub fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

#[must_use]
pub fn without_inline_file_data(value: &Value) -> Value {
    match value {
        Value::Object(m) => {
            if m.get("schema") == Some(&json!(INLINE_FILE_SCHEMA))
                && matches!(m.get("data_base64"), Some(Value::String(_)))
            {
                let mut out: Map<String, Value> = m
                    .iter()
                    .filter(|(k, _)| *k != "data_base64")
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                out.insert(
                    "data_base64_omitted".into(),
                    json!("complete base64 in structuredContent at this path"),
                );
                return Value::Object(out);
            }
            Value::Object(m.iter().map(|(k, v)| (k.clone(), without_inline_file_data(v))).collect())
        }
        Value::Array(a) => Value::Array(a.iter().map(without_inline_file_data).collect()),
        other => other.clone(),
    }
}

fn b64_python() -> GeneralPurpose {

    GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        GeneralPurposeConfig::new()
            .with_decode_allow_trailing_bits(true)
            .with_decode_padding_mode(DecodePaddingMode::RequireCanonical),
    )
}

fn image_is_valid(image: &Map<String, Value>, decoded: &[u8]) -> bool {
    let int = |k: &str| match image.get(k) {
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => n.as_i64(),
        _ => None,
    };
    let (Some(width), Some(height)) = (int("width_px"), int("height_px")) else { return false };

    let animated = image.get("extension") == Some(&json!(RUST_EXTENSION))
        && match image.get("mime_type").and_then(Value::as_str) {
            Some("image/webp") => {
                decoded.len() > 12 && &decoded[..4] == b"RIFF" && &decoded[8..12] == b"WEBP"
            }
            Some("image/gif") => decoded.starts_with(b"GIF89a") || decoded.starts_with(b"GIF87a"),
            _ => false,
        };
    if animated {
        return image.get("bytes").and_then(Value::as_f64) == Some(decoded.len() as f64)
            && decoded.len() <= MAX_IMAGE_BYTES
            && width > 0
            && height > 0
            && width.checked_mul(height).is_some_and(|p| p <= MAX_IMAGE_PIXELS)
            && image.get("sha256") == Some(&json!(hex::encode(Sha256::digest(decoded))));
    }
    image.get("mime_type") == Some(&json!("image/png"))
        && image.get("bytes").and_then(Value::as_f64) == Some(decoded.len() as f64)
        && decoded.len() <= MAX_IMAGE_BYTES
        && width > 0
        && height > 0
        && width.checked_mul(height).is_some_and(|p| p <= MAX_IMAGE_PIXELS)
        && decoded.starts_with(b"\x89PNG\r\n\x1a\n")
        && image.get("sha256") == Some(&json!(hex::encode(Sha256::digest(decoded))))
}



pub fn tool_result(response: &Map<String, Value>, is_error: bool) -> Result<Value, BridgeError> {
    let mut structured = response.clone();
    let mut image_content = None;
    if let Some(Value::Object(payload)) = structured.get("result")
        && let Some(Value::Object(image)) = payload.get("image")
    {
        let mut image = image.clone();
        let Some(Value::String(encoded)) = image.shift_remove("data_base64") else {
            return Err(bridge("inline image lacks base64 data"));
        };
        let decoded = b64_python()
            .decode(encoded.as_bytes())
            .map_err(|_| bridge("inline image is not canonical base64"))?;
        if !image_is_valid(&image, &decoded) {
            return Err(bridge("inline image failed validation"));
        }
        let mime = image.get("mime_type").and_then(Value::as_str).unwrap_or("image/png").to_owned();
        let mut clean = payload.clone();
        clean.insert("image".into(), Value::Object(image));
        structured.insert("result".into(), Value::Object(clean));
        image_content = Some(json!({"type": "image", "data": encoded, "mimeType": mime}));
    }
    let structured = Value::Object(structured);
    let mut content =
        vec![json!({"type": "text", "text": compact_json(&without_inline_file_data(&structured))})];
    content.extend(image_content);
    Ok(json!({"content": content, "structuredContent": structured, "isError": is_error}))
}

fn only_keys(params: &Map<String, Value>, allowed: &[&str]) -> bool {
    params.keys().all(|k| allowed.contains(&k.as_str()))
}

fn dispatch<T: Transport>(
    client: &mut PublicAgentClient<T>,
    id: &Value,
    method: Option<&Value>,
    params: &Value,
) -> Result<Value, BridgeError> {
    let invalid = |m: &str| BridgeError::InvalidParams(m.to_owned());
    match method.and_then(Value::as_str) {
        Some("initialize") => {
            if !params.is_object() {
                return Err(invalid("initialize params must be an object"));
            }
            Ok(rpc_result(
                id,
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {"tools": {"listChanged": false}, "resources": {}},
                    "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
                    "instructions": "Read implexity://manual with resources/read. Call tools/list before each action sequence. Use the listed input schemas.",
                }),
            ))
        }
        Some("ping") => Ok(rpc_result(id, json!({}))),
        Some("resources/list") => {
            if !params.as_object().is_some_and(|p| only_keys(p, &["_meta"])) {
                return Err(invalid("resources/list accepts no cursor; the resource list is complete"));
            }
            Ok(rpc_result(
                id,
                json!({"resources": [{
                    "uri": MANUAL_URI, "name": "Implexity engineering manual",
                    "description": "Read this manual before you call tools. It gives workflows, input schemas, permissions, and recovery steps.",
                    "mimeType": "application/json",
                }]}),
            ))
        }
        Some("resources/read") => {
            let Some(p) = params.as_object().filter(|p| only_keys(p, &["uri", "_meta"])) else {
                return Err(invalid("resources/read requires a resource URI"));
            };
            let Some(Value::String(uri)) = p.get("uri") else {
                return Err(invalid("resources/read requires a resource URI"));
            };
            if uri != MANUAL_URI {
                return Ok(rpc_error(id, -32002, "Resource not found"));
            }
            let text = client.read_manual()?;
            Ok(rpc_result(
                id,
                json!({"contents": [{"uri": MANUAL_URI, "mimeType": "application/json", "text": text}]}),
            ))
        }
        Some("tools/list") => Ok(rpc_result(id, json!({"tools": client.list_tools()?}))),
        Some("tools/call") => {
            let Some(p) = params.as_object() else {
                return Err(invalid("tools/call params must be an object"));
            };
            let empty = Value::Object(Map::new());
            let arguments = p.get("arguments").unwrap_or(&empty);
            let (Some(Value::String(name)), Value::Object(arguments)) = (p.get("name"), arguments) else {
                return Err(invalid("tools/call requires a name and object arguments"));
            };
            match client.call_tool(name, arguments) {
                Err(BridgeError::ActionHttp { body, .. }) => Ok(rpc_result(id, tool_result(&body, true)?)),
                Err(e) => Err(e),
                Ok(response) => {
                    let is_error = response.get("ok") != Some(&Value::Bool(true));
                    Ok(rpc_result(id, tool_result(&response, is_error)?))
                }
            }
        }
        _ => Ok(rpc_error(
            id,
            -32601,
            &format!("Method not found: {}", py_str(method.unwrap_or(&Value::Null))),
        )),
    }
}

pub fn handle<T: Transport>(client: &mut PublicAgentClient<T>, message: &Value) -> Option<Value> {
    let Value::Object(m) = message else {
        return Some(rpc_error(&Value::Null, -32600, "Invalid Request"));
    };
    if m.get("jsonrpc") != Some(&json!("2.0")) {
        return Some(rpc_error(m.get("id").unwrap_or(&Value::Null), -32600, "Invalid Request"));
    }
    let id = m.get("id")?.clone();
    let empty = Value::Object(Map::new());
    let params = m.get("params").unwrap_or(&empty);
    Some(match dispatch(client, &id, m.get("method"), params) {
        Ok(v) => v,
        Err(BridgeError::InvalidParams(msg)) => rpc_error(&id, -32602, &msg),
        Err(BridgeError::Bridge(msg)) => rpc_error(&id, -32603, &msg),
        Err(e @ BridgeError::ActionHttp { .. }) => rpc_error(&id, -32603, &e.to_string()),
    })
}



pub fn serve<T: Transport, R: BufRead, W: Write>(
    client: &mut PublicAgentClient<T>,
    input: R,
    mut output: W,
) -> std::io::Result<()> {
    for line in input.split(b'\n') {
        let line = line?;
        if String::from_utf8_lossy(&line).trim().is_empty() {
            continue;
        }
        let response = match strict_json(&line, "MCP request") {
            Ok(message) => handle(client, &message),
            Err(e) => Some(rpc_error(&Value::Null, -32700, &format!("Parse error: {e}"))),
        };
        if let Some(r) = response {

            let mut framed = compact_json(&r).into_bytes();
            framed.push(b'\n');
            output.write_all(&framed)?;
            output.flush()?;
        }
    }
    Ok(())
}



pub fn client_from_environment() -> Result<PublicAgentClient<HttpTransport>, BridgeError> {
    let url = base_url(std::env::var("IMPLEXITY_MCP_BASE_URL").ok().as_deref())?;
    let timeout = timeout_seconds(std::env::var("IMPLEXITY_MCP_TIMEOUT_SECONDS").ok().as_deref())?;
    Ok(PublicAgentClient::new(url, HttpTransport::new(timeout)))
}

