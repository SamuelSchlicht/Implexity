// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::ws::{Client, Incoming};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CdpError(pub String);

impl std::fmt::Display for CdpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn fail(m: impl Into<String>) -> CdpError {
    CdpError(m.into())
}

fn is_file(p: &Path) -> bool {
    p.is_file()
}

pub const PLAYWRIGHT_CHROMIUM_LAYOUTS: [&str; 8] = [
    "chrome-linux/chrome",
    "chrome-linux64/chrome",
    "chrome-mac/Chromium.app/Contents/MacOS/Chromium",
    "chrome-mac-arm64/Chromium.app/Contents/MacOS/Chromium",
    "chrome-mac/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
    "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
    "chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
    "chrome-win/chrome.exe",
];


#[must_use]
pub fn find_chromium() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("IMPLEXITY_CHROMIUM").map(PathBuf::from).filter(|p| is_file(p)) {
        return Some(p);
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os("PLAYWRIGHT_BROWSERS_PATH").filter(|p| !p.is_empty()) {
        roots.push(PathBuf::from(p));
    }
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(Path::new(&home).join(".cache").join("ms-playwright"));
        roots.push(Path::new(&home).join("Library").join("Caches").join("ms-playwright"));
    }
    if let Some(found) = find_in_playwright_roots(&roots) {
        return Some(found);
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for name in ["chromium", "chromium-browser", "google-chrome", "google-chrome-stable"] {
            let candidate = dir.join(name);
            if is_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

#[must_use]
pub fn find_in_playwright_roots(roots: &[PathBuf]) -> Option<PathBuf> {
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else { continue };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("chromium-")))
            .collect();
        dirs.sort();
        for dir in dirs.iter().rev() {
            for rel in PLAYWRIGHT_CHROMIUM_LAYOUTS {
                let candidate = dir.join(rel);
                if is_file(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

pub(crate) struct Browser {
    child: Child,
    profile: PathBuf,
    client: Client,
    next_id: u64,
    session: Option<String>,
    allowed_origin: String,
    events: Vec<Value>,
    pub version: String,
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.profile);
    }
}

impl Browser {
    pub(crate) fn launch(
        executable: &Path,
        software_webgl: bool,
        allowed_origin: &str,
    ) -> Result<Self, CdpError> {
        let profile =
            std::env::temp_dir().join(format!("implexity-capture-{}", implexity_io::atomic::unique_token()));
        std::fs::create_dir_all(&profile).map_err(|e| fail(format!("browser profile: {e}")))?;
        let mut cmd = Command::new(executable);
        cmd.arg("--headless=new")
            .arg("--remote-debugging-port=0")
            .arg(format!("--user-data-dir={}", profile.display()))
            .args([
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-background-networking",
                "--disable-component-update",
                "--disable-default-apps",
                "--disable-extensions",
                "--disable-sync",
                "--disable-dev-shm-usage",
                "--hide-scrollbars",
                "--mute-audio",
                "--no-sandbox",
            ]);
        if software_webgl {
            cmd.args(["--use-gl=angle", "--use-angle=swiftshader", "--enable-unsafe-swiftshader"]);
        }
        cmd.arg("about:blank").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
        let mut child =
            cmd.spawn().map_err(|e| fail(format!("could not start {}: {e}", executable.display())))?;
        let stderr = child.stderr.take().ok_or_else(|| fail("browser stderr unavailable"))?;
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::Builder::new()
            .name("chromium-stderr".into())
            .spawn(move || {
                let mut announced = false;
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if !announced && let Some(at) = line.find("ws://") {
                        let _ = tx.send(line[at..].trim().to_owned());
                        announced = true;
                    }
                }
            })
            .map_err(|e| fail(e.to_string()))?;
        let Ok(url) = rx.recv_timeout(Duration::from_secs(30)) else {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_dir_all(&profile);
            return Err(fail("the browser did not announce its DevTools endpoint within 30 s"));
        };
        let client = match Client::connect(&url) {
            Ok(c) => c,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_dir_all(&profile);
                return Err(fail(format!("DevTools connection failed: {e}")));
            }
        };
        let mut browser = Self {
            child,
            profile,
            client,
            next_id: 0,
            session: None,
            allowed_origin: allowed_origin.to_owned(),
            events: Vec::new(),
            version: String::new(),
        };
        let v = browser.command("Browser.getVersion", json!({}), false, Duration::from_secs(30))?;
        let product = v.get("product").and_then(Value::as_str).unwrap_or_default();
        product.split_once('/').map_or(product, |(_, ver)| ver).clone_into(&mut browser.version);
        Ok(browser)
    }

    pub(crate) fn open_page(&mut self) -> Result<(), CdpError> {
        let t = Duration::from_secs(30);
        let ctx = self.command("Target.createBrowserContext", json!({}), false, t)?;
        let context_id = ctx.get("browserContextId").cloned().unwrap_or(Value::Null);
        let target = self.command(
            "Target.createTarget",
            json!({"url": "about:blank", "browserContextId": context_id}),
            false,
            t,
        )?;
        let target_id = target.get("targetId").cloned().ok_or_else(|| fail("no page target"))?;
        let attached =
            self.command("Target.attachToTarget", json!({"targetId": target_id, "flatten": true}), false, t)?;
        self.session = attached.get("sessionId").and_then(Value::as_str).map(str::to_owned);
        if self.session.is_none() {
            return Err(fail("the page session could not be attached"));
        }
        self.command("Fetch.enable", json!({"patterns": [{"urlPattern": "*"}]}), true, t)?;
        self.command("Page.enable", json!({}), true, t)?;
        self.command("Runtime.enable", json!({}), true, t)?;
        Ok(())
    }

    fn origin_of(url: &str) -> Option<&str> {
        let rest = url.split_once("://")?.1;
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let scheme_len = url.len() - rest.len();
        Some(&url[..scheme_len + end])
    }

    fn handle_event(&mut self, event: &Value) -> Result<(), CdpError> {
        match event.get("method").and_then(Value::as_str) {
            Some("Fetch.requestPaused") => {
                let params = &event["params"];
                let id = params["requestId"].clone();
                let url = params["request"]["url"].as_str().unwrap_or_default();
                let allowed = Self::origin_of(url) == Some(self.allowed_origin.as_str());
                let (method, body) = if allowed {
                    ("Fetch.continueRequest", json!({"requestId": id}))
                } else {
                    ("Fetch.failRequest", json!({"requestId": id, "errorReason": "Aborted"}))
                };
                self.send(method, &body, true)?;
                Ok(())
            }
            Some("Inspector.targetCrashed" | "Target.targetCrashed") => Err(fail("the page crashed")),
            Some("Target.detachedFromTarget") => Err(fail("the page target detached")),
            _ => {
                if self.events.len() < 256 {
                    self.events.push(event.clone());
                }
                Ok(())
            }
        }
    }

    fn send(&mut self, method: &str, params: &Value, in_session: bool) -> Result<u64, CdpError> {
        self.next_id += 1;
        let id = self.next_id;
        let mut msg = json!({"id": id, "method": method, "params": params});
        if in_session && let Some(s) = &self.session {
            msg["sessionId"] = json!(s);
        }
        self.client.send_text(&msg.to_string()).map_err(|e| fail(format!("{method}: {e}")))?;
        Ok(id)
    }

    #[allow(clippy::needless_pass_by_value)]
    pub(crate) fn command(
        &mut self,
        method: &str,
        params: Value,
        in_session: bool,
        timeout: Duration,
    ) -> Result<Value, CdpError> {
        let id = self.send(method, &params, in_session)?;
        let deadline = Instant::now() + timeout;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(fail(format!("{method} timed out after {:.0} s", timeout.as_secs_f64())));
            }
            let incoming = self
                .client
                .recv((deadline - now).min(Duration::from_millis(250)))
                .map_err(|e| fail(format!("{method}: {e}")))?;
            let Incoming::Text(text) = incoming else { continue };
            let msg: Value =
                serde_json::from_str(&text).map_err(|e| fail(format!("DevTools sent invalid JSON: {e}")))?;
            if msg.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(err) = msg.get("error") {
                    let message = err.get("message").and_then(Value::as_str).unwrap_or("error");
                    return Err(fail(format!("{method}: {message}")));
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }
            if msg.get("method").is_some() {
                self.handle_event(&msg)?;
            }
        }
    }

    pub(crate) fn pump(&mut self, duration: Duration) -> Result<(), CdpError> {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            if let Incoming::Text(text) =
                self.client.recv(left.min(Duration::from_millis(100))).map_err(|e| fail(e.to_string()))?
                && let Ok(msg) = serde_json::from_str::<Value>(&text)
                && msg.get("method").is_some()
            {
                self.handle_event(&msg)?;
            }
        }
        Ok(())
    }

    pub(crate) fn wait_event(&mut self, method: &str, timeout: Duration) -> Result<Value, CdpError> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(pos) =
                self.events.iter().position(|e| e.get("method").and_then(Value::as_str) == Some(method))
            {
                return Ok(self.events.remove(pos));
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(fail(format!("{method} did not arrive within {:.0} s", timeout.as_secs_f64())));
            }
            if let Incoming::Text(text) = self
                .client
                .recv((deadline - now).min(Duration::from_millis(250)))
                .map_err(|e| fail(e.to_string()))?
            {
                let msg: Value = serde_json::from_str(&text)
                    .map_err(|e| fail(format!("DevTools sent invalid JSON: {e}")))?;
                if msg.get("method").is_some() {
                    self.handle_event(&msg)?;
                }
            }
        }
    }

    pub(crate) fn clear_events(&mut self) {
        self.events.clear();
    }

    pub(crate) fn evaluate(&mut self, expression: &str, timeout: Duration) -> Result<Value, CdpError> {
        let r = self.command(
            "Runtime.evaluate",
            json!({"expression": expression, "awaitPromise": true, "returnByValue": true}),
            true,
            timeout,
        )?;
        if let Some(ex) = r.get("exceptionDetails") {
            let text = ex
                .get("exception")
                .and_then(|e| e.get("description"))
                .and_then(Value::as_str)
                .or_else(|| ex.get("text").and_then(Value::as_str))
                .unwrap_or("evaluation failed");
            return Err(fail(text.to_owned()));
        }
        Ok(r.get("result").and_then(|v| v.get("value")).cloned().unwrap_or(Value::Null))
    }

    pub(crate) fn wait_for(&mut self, expression: &str, timeout: Duration) -> Result<(), CdpError> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(fail(format!(
                    "Timeout {}ms exceeded while waiting for the viewer",
                    timeout.as_millis()
                )));
            }
            let value =
                self.evaluate(&format!("Boolean({expression})"), left.min(Duration::from_secs(30)))?;
            if value == Value::Bool(true) {
                return Ok(());
            }
            self.pump(Duration::from_millis(100))?;
        }
    }
}

