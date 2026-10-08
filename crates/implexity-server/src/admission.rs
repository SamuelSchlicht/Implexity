// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::fmt::Write as _;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

pub const MAX_BODY_BYTES: u64 = 64 * 1024 * 1024;

pub const SINGLETON_HEADERS: [&str; 14] = [
    "Host",
    "Origin",
    "Content-Length",
    "Content-Type",
    "Transfer-Encoding",
    "Expect",
    "Sec-Fetch-Site",
    "Sec-Fetch-Mode",
    "Sec-Fetch-Dest",
    "Sec-Fetch-User",
    "Upgrade",
    "Connection",
    "Sec-WebSocket-Version",
    "Sec-WebSocket-Key",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejection {
    pub status: u16,
    pub message: String,
    pub headers: Vec<(String, String)>,
}

impl Rejection {
    #[must_use]
    pub fn new(status: u16, message: &str) -> Self {
        Self { status, message: message.to_owned(), headers: Vec::new() }
    }
}

#[derive(Clone, Debug)]
pub struct RequestHead<'a> {
    pub method: &'a str,
    pub target: &'a str,
    pub headers: &'a [(String, Vec<u8>)],
}

impl RequestHead<'_> {
    #[must_use]
    pub fn get_all(&self, name: &str) -> Vec<String> {
        self.headers.iter().filter(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| latin1(v)).collect()
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<String> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| latin1(v))
    }

    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case(name))
    }
}

#[derive(Clone, Debug)]
pub struct BindInfo {
    pub configured_host: String,
    pub bound: SocketAddr,
    pub local: SocketAddr,
}

#[must_use]
pub fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidAddress(pub String);

fn invalid(msg: &str) -> InvalidAddress {
    InvalidAddress(msg.to_owned())
}



pub fn ip_address(value: &str) -> Result<IpAddr, InvalidAddress> {
    if value.contains(':') {
        return value.parse::<Ipv6Addr>().map(IpAddr::V6).map_err(|_| invalid("not an IPv6 address"));
    }
    value.parse::<Ipv4Addr>().map(IpAddr::V4).map_err(|_| invalid("not an IPv4 address"))
}

#[must_use]
pub fn compressed(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => {
            let seg = v6.segments();
            let (mut best, mut best_len, mut cur, mut cur_len) = (usize::MAX, 0usize, 0usize, 0usize);
            for (i, &s) in seg.iter().enumerate() {
                if s == 0 {
                    if cur_len == 0 {
                        cur = i;
                    }
                    cur_len += 1;
                    if cur_len > best_len {
                        best_len = cur_len;
                        best = cur;
                    }
                } else {
                    cur_len = 0;
                }
            }
            let hex: Vec<String> = seg.iter().map(|s| format!("{s:x}")).collect();
            if best_len > 1 {
                let head = hex[..best].join(":");
                let tail = hex[best + best_len..].join(":");
                format!("{head}::{tail}")
            } else {
                hex.join(":")
            }
        }
    }
}

fn is_ldh_label(label: &str) -> bool {
    let b = label.as_bytes();
    let alnum = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    match b.len() {
        0 => false,
        1 => alnum(b[0]),
        n => n <= 63 && alnum(b[0]) && alnum(b[n - 1]) && b[1..n - 1].iter().all(|&c| alnum(c) || c == b'-'),
    }
}



pub fn host_name(value: &str) -> Result<String, InvalidAddress> {
    let value = value.to_lowercase();
    if let Ok(ip) = ip_address(&value) {
        return Ok(compressed(ip));
    }
    if !value.is_ascii() {
        return Err(invalid("non-ASCII host names are not accepted"));
    }

    if !value.is_empty() {
        let labels: Vec<&str> = value.split('.').collect();
        if let Some((last, init)) = labels.split_last()
            && (init.iter().any(|l| l.is_empty() || l.len() >= 64) || last.len() >= 64)
        {
            return Err(invalid("label empty or too long"));
        }
    }
    let value = value.strip_suffix('.').unwrap_or(&value);
    if value.len() > 253 || value.is_empty() || !value.split('.').all(is_ldh_label) {
        return Err(invalid("invalid host name"));
    }
    Ok(value.to_owned())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitUrl {
    pub scheme: String,
    pub netloc: String,
    pub path: String,
    pub query: String,
    pub fragment: String,
}

fn check_bracketed_host(hostname: &str) -> Result<(), InvalidAddress> {
    if let Some(rest) = hostname.strip_prefix('v') {

        let hex_len = rest.bytes().take_while(u8::is_ascii_hexdigit).count();
        let after = &rest[hex_len..];
        let ok = hex_len > 0 && after.starts_with('.') && after.len() > 1 && !after[1..].contains('\n');
        return if ok { Ok(()) } else { Err(invalid("IPvFuture address is invalid")) };
    }
    match ip_address(hostname)? {
        IpAddr::V4(_) => Err(invalid("An IPv4 address cannot be in brackets")),
        IpAddr::V6(_) => Ok(()),
    }
}



pub fn urlsplit(url: &str) -> Result<SplitUrl, InvalidAddress> {
    let mut rest = url;
    let mut scheme = String::new();
    if let Some(i) = rest.find(':') {
        let first = rest.chars().next();
        if i > 0
            && first.is_some_and(|c| c.is_ascii_alphabetic())
            && rest[..i].chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        {
            scheme = rest[..i].to_ascii_lowercase();
            rest = &rest[i + 1..];
        }
    }
    let mut netloc = String::new();
    if let Some(after) = rest.strip_prefix("//") {
        let delim = after.find(['/', '?', '#']).unwrap_or(after.len());
        after[..delim].clone_into(&mut netloc);
        rest = &after[delim..];
        if netloc.contains('[') != netloc.contains(']') {
            return Err(invalid("Invalid IPv6 URL"));
        }
        if netloc.contains('[') {
            let host_port = netloc.rsplit_once('@').map_or(netloc.as_str(), |(_, h)| h);
            let hostname = if let Some((before, bracketed)) = host_port.split_once('[') {
                if !before.is_empty() {
                    return Err(invalid("Invalid IPv6 URL"));
                }
                let (hostname, port) = bracketed.split_once(']').unwrap_or((bracketed, ""));
                if !port.is_empty() && !port.starts_with(':') {
                    return Err(invalid("Invalid IPv6 URL"));
                }
                hostname
            } else {
                host_port.split_once(':').map_or(host_port, |(h, _)| h)
            };
            check_bracketed_host(hostname)?;
        }
    }
    let (rest, fragment) = rest.split_once('#').unwrap_or((rest, ""));
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    Ok(SplitUrl {
        scheme,
        netloc,
        path: path.to_owned(),
        query: query.to_owned(),
        fragment: fragment.to_owned(),
    })
}

impl SplitUrl {
    fn hostinfo(&self) -> (&str, Option<&str>) {
        let hostinfo = self.netloc.rsplit_once('@').map_or(self.netloc.as_str(), |(_, h)| h);
        let (hostname, port) = if let Some((_, bracketed)) = hostinfo.split_once('[') {
            let (hostname, after) = bracketed.split_once(']').unwrap_or((bracketed, ""));
            (hostname, after.split_once(':').map_or("", |(_, p)| p))
        } else {
            hostinfo.split_once(':').unwrap_or((hostinfo, ""))
        };
        (hostname, (!port.is_empty()).then_some(port))
    }

    #[must_use]
    pub fn hostname(&self) -> Option<String> {
        let (hostname, _) = self.hostinfo();
        if hostname.is_empty() {
            return None;
        }
        Some(match hostname.split_once('%') {
            Some((h, zone)) => format!("{}%{zone}", h.to_lowercase()),
            None => hostname.to_lowercase(),
        })
    }



    pub fn port(&self) -> Result<Option<u32>, InvalidAddress> {
        let (_, port) = self.hostinfo();
        let Some(port) = port else { return Ok(None) };
        if !port.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid("Port could not be cast to integer value"));
        }
        let trimmed = port.trim_start_matches('0');
        if trimmed.len() > 5 {
            return Err(invalid("Port out of range 0-65535"));
        }
        let value: u32 = if trimmed.is_empty() { 0 } else { trimmed.parse().map_err(|_| invalid("port"))? };
        if value > 65535 {
            return Err(invalid("Port out of range 0-65535"));
        }
        Ok(Some(value))
    }
}

fn is_py_space_or_control(c: char) -> bool {
    let u = u32::from(c);
    c.is_whitespace() || u < 32 || u == 127 || (0x1c..=0x1f).contains(&u)
}



pub fn authority(value: &str, origin: bool) -> Result<(String, u32), InvalidAddress> {
    let value = value.trim_matches([' ', '\t']);
    if value.is_empty()
        || value.chars().any(is_py_space_or_control)
        || value.chars().any(|c| "/\\@?#%,".contains(c))
        || value.ends_with(':')
    {
        return Err(invalid("invalid authority"));
    }
    let parsed = urlsplit(&format!("//{value}"))?;
    let Some(hostname) = parsed.hostname() else {
        return Err(invalid("host is required"));
    };
    let port = parsed.port()?.unwrap_or(80);
    if !(1..=65535).contains(&port) {
        return Err(invalid("invalid port"));
    }
    let mut host = host_name(&hostname)?;
    if origin && hostname.ends_with('.') {
        host.push('.');
    }
    Ok((host, port))
}

#[must_use]
pub fn loopback(host: &str) -> bool {
    host == "localhost" || ip_address(host).is_ok_and(|ip| ip.is_loopback())
}

fn is_token(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+.^_`|~-".contains(&b))
}

fn reject(status: u16, message: &str) -> Rejection {
    Rejection::new(status, message)
}



pub fn check_request(head: &RequestHead<'_>, bind: &BindInfo) -> Result<u64, Rejection> {
    for (name, value) in head.headers {
        if !is_token(name) || value.iter().any(|&b| (b < 32 && b != b'\t') || b == 127) {
            return Err(reject(400, "invalid request header"));
        }
    }
    for name in SINGLETON_HEADERS {
        let values = head.get_all(name);
        if values.len() > 1 || values.iter().any(|v| v.contains(['\r', '\n'])) {
            return Err(reject(400, &format!("invalid or duplicate {name}")));
        }
    }
    if head.has("Transfer-Encoding") {
        return Err(reject(400, "Transfer-Encoding is unsupported; use Content-Length"));
    }
    let length = head.get("Content-Length").unwrap_or_else(|| "0".to_owned());
    if length.is_empty() || !length.bytes().all(|b| b.is_ascii_digit()) || length.len() > 10 {
        return Err(reject(400, "invalid Content-Length"));
    }
    let body_length: u64 = length.parse().map_err(|_| reject(400, "invalid Content-Length"))?;
    if body_length > MAX_BODY_BYTES {
        return Err(reject(413, "request body exceeds 64 MiB"));
    }
    if matches!(head.method, "GET" | "HEAD" | "OPTIONS") && body_length > 0 {
        return Err(reject(400, "this request method does not accept a body"));
    }
    let expect = head.get("Expect").unwrap_or_default().to_lowercase();
    if !(expect.is_empty() || expect == "100-continue") {
        return Err(reject(417, "unsupported Expect header"));
    }
    if !head.target.starts_with('/') || head.target.starts_with("//") {
        return Err(reject(400, "use an origin-form request target"));
    }
    check_host_and_origin(head, bind)
        .map_err(|e| e.unwrap_or_else(|| reject(400, "invalid Host or Origin")))?;
    Ok(body_length)
}

fn check_host_and_origin(head: &RequestHead<'_>, bind: &BindInfo) -> Result<(), Option<Rejection>> {
    let host_header = head.get("Host").unwrap_or_default();
    let authority_ = authority(&host_header, false).map_err(|_| None)?;
    let bound_host = host_name(&bind.bound.ip().to_string()).map_err(|_| None)?;
    let configured = if bind.configured_host.is_empty() {
        bound_host.clone()
    } else {
        host_name(&bind.configured_host).map_err(|_| None)?
    };
    let port = u32::from(bind.bound.port());
    let is_wildcard = |h: &str| h == "0.0.0.0" || h == "::";
    let wildcard = is_wildcard(&configured);
    let mut allowed: Vec<String> =
        [configured.clone(), bound_host.clone()].into_iter().filter(|h| !is_wildcard(h)).collect();
    if wildcard || loopback(&configured) || loopback(&bound_host) {
        allowed.extend(["localhost", "127.0.0.1", "::1"].map(str::to_owned));
    }
    if wildcard {
        let local = host_name(&bind.local.ip().to_string()).map_err(|_| None)?;
        if !is_wildcard(&local) {
            allowed.push(local);
        }
    }
    if !allowed.contains(&authority_.0) || authority_.1 != port {
        return Err(Some(reject(403, "Host does not match the configured service address and port")));
    }
    let has_origin = head.has("Origin");
    let browser =
        has_origin || head.headers.iter().any(|(n, _)| n.to_ascii_lowercase().starts_with("sec-fetch-"));
    if browser && wildcard && !loopback(&authority_.0) {
        return Err(Some(reject(
            403,
            "browser access to a wildcard bind requires localhost; bind an explicit address for another origin",
        )));
    }
    if has_origin {
        let origin = head.get("Origin").unwrap_or_default();
        let refused = || Some(reject(403, "Origin must match the service URL; open the GUI from that URL"));

        let cleaned: String = origin
            .trim_start_matches(|c: char| c <= ' ')
            .chars()
            .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
            .collect();
        let parsed = urlsplit(&cleaned).map_err(|_| None)?;
        if origin.chars().any(is_py_space_or_control) {
            return Err(refused());
        }
        let mut expected = String::from("http://");
        let _ = write!(expected, "{}", parsed.netloc);
        if origin != expected
            || parsed.scheme != "http"
            || parsed.netloc.is_empty()
            || !parsed.path.is_empty()
            || !parsed.query.is_empty()
            || !parsed.fragment.is_empty()
        {
            return Err(refused());
        }
        let lhs = authority(&parsed.netloc, true).map_err(|_| None)?;
        let rhs = authority(&host_header, true).map_err(|_| None)?;
        if lhs != rhs {
            return Err(refused());
        }
    }
    if let Some(site) = head.get("Sec-Fetch-Site")
        && site != "same-origin"
        && site != "none"
    {
        return Err(Some(reject(403, "cross-site browser requests are not accepted")));
    }
    Ok(())
}

