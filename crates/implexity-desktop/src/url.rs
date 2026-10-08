// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


#[derive(Debug, Default, PartialEq, Eq)]
struct Split {
    scheme: String,
    hostname: Option<String>,
    port: Option<u32>,
    has_userinfo: bool,
    path: String,
    query: String,
    fragment: String,
}

fn scheme_of(url: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = url.split_once(':')?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    (first.is_ascii_alphabetic() && chars.all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)))
        .then_some((scheme, rest))
}

fn urlsplit(url: &str) -> Result<Split, ()> {
    let url = url.trim_start_matches(|c: char| c <= ' ');
    let url: String = url.chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let (scheme, mut rest) = match scheme_of(&url) {
        Some((s, r)) => (s.to_ascii_lowercase(), r.to_owned()),
        None => (String::new(), url.clone()),
    };
    let mut netloc = String::new();
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        after[..end].clone_into(&mut netloc);
        rest = after[end..].to_owned();
        if netloc.contains('[') != netloc.contains(']') {
            return Err(());
        }
    }
    let (rest, fragment) = rest.split_once('#').map_or((rest.as_str(), ""), |(a, b)| (a, b));
    let (path, query) = rest.split_once('?').map_or((rest, ""), |(a, b)| (a, b));
    let has_userinfo = netloc.contains('@');
    let hostinfo = netloc.rsplit_once('@').map_or(netloc.as_str(), |(_, h)| h);
    let (host, port) = if let Some(bracketed) = hostinfo.split_once('[').map(|(_, b)| b) {
        let (h, after) = bracketed.split_once(']').unwrap_or((bracketed, ""));
        (h.to_owned(), after.split_once(':').map(|(_, p)| p.to_owned()))
    } else {
        match hostinfo.split_once(':') {
            Some((h, p)) => (h.to_owned(), Some(p.to_owned())),
            None => (hostinfo.to_owned(), None),
        }
    };
    let port = match port {
        None => None,
        Some(p) if p.is_empty() => None,
        Some(p) => {
            if !p.bytes().all(|b| b.is_ascii_digit()) {
                return Err(());
            }
            let n: u32 = p.parse().map_err(|_| ())?;
            if n > 65535 {
                return Err(());
            }
            Some(n)
        }
    };
    Ok(Split {
        scheme,
        hostname: (!host.is_empty()).then(|| host.to_lowercase()),
        port,
        has_userinfo,
        path: path.to_owned(),
        query: query.to_owned(),
        fragment: fragment.to_owned(),
    })
}



pub(crate) fn local_viewer_url(value: &str) -> Result<String, String> {
    let parsed = urlsplit(value).map_err(|()| "Use http://127.0.0.1:PORT".to_owned())?;
    let refused = parsed.scheme != "http"
        || parsed.hostname.as_deref() != Some("127.0.0.1")
        || !parsed.port.is_some_and(|p| (1..=65535).contains(&p))
        || parsed.has_userinfo
        || !(parsed.path.is_empty() || parsed.path == "/")
        || !parsed.query.is_empty()
        || !parsed.fragment.is_empty();
    match parsed.port {
        Some(port) if !refused => Ok(format!("http://127.0.0.1:{port}/viewer/model.html")),
        _ => Err("Use a loopback service address: http://127.0.0.1:PORT".to_owned()),
    }
}

