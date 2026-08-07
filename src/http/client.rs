//! Building and sending a [`SavedRequest`], and capturing the response.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};

use super::vars::substitute;
use crate::model::{Method, SavedRequest};

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub reason: String,
    pub elapsed: Duration,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub size: usize,
}

impl HttpResponse {
    pub fn status_line(&self) -> String {
        let ms = self.elapsed.as_millis();
        format!(
            "{} {} · {}ms · {}",
            self.status,
            self.reason,
            ms,
            human_size(self.size)
        )
    }
}

fn human_size(n: usize) -> String {
    if n < 1024 {
        format!("{n}B")
    } else if n < 1024 * 1024 {
        format!("{:.1}kB", n as f64 / 1024.0)
    } else {
        format!("{:.1}MB", n as f64 / 1024.0 / 1024.0)
    }
}

/// Send a request against `base_url`, applying `{{variable}}` substitution and
/// `{pathParam}` replacement. `bearer`, when present, sets the Authorization
/// header unless the request already defines one.
pub async fn send_request(
    client: &reqwest::Client,
    base_url: &str,
    req: &SavedRequest,
    vars: &HashMap<String, String>,
    bearer: Option<&str>,
) -> Result<HttpResponse> {
    let url = build_url(base_url, req, vars);

    let mut headers = HeaderMap::new();
    let mut has_auth = false;
    let mut has_content_type = false;
    for row in req
        .headers
        .iter()
        .filter(|r| r.enabled && !r.key.is_empty())
    {
        let name = HeaderName::from_bytes(substitute(&row.key, vars).as_bytes())
            .map_err(|e| anyhow!("invalid header name {:?}: {e}", row.key))?;
        let value = HeaderValue::from_str(&substitute(&row.value, vars))
            .map_err(|e| anyhow!("invalid value for header {:?}: {e}", row.key))?;
        if name == AUTHORIZATION {
            has_auth = true;
        }
        if name == CONTENT_TYPE {
            has_content_type = true;
        }
        headers.insert(name, value);
    }

    let method = match req.method {
        Method::Get => reqwest::Method::GET,
        Method::Post => reqwest::Method::POST,
        Method::Put => reqwest::Method::PUT,
        Method::Patch => reqwest::Method::PATCH,
        Method::Delete => reqwest::Method::DELETE,
        Method::Head => reqwest::Method::HEAD,
        Method::Options => reqwest::Method::OPTIONS,
    };

    let query: Vec<(String, String)> = req
        .query
        .iter()
        .filter(|r| r.enabled && !r.key.is_empty())
        .map(|r| (substitute(&r.key, vars), substitute(&r.value, vars)))
        .collect();

    let mut rb = client.request(method, &url).headers(headers).query(&query);

    if let Some(token) = bearer.filter(|_| !has_auth) {
        rb = rb.bearer_auth(token);
    }

    if let Some(body) = req.body.as_ref().filter(|b| !b.trim().is_empty()) {
        rb = rb.body(substitute(body, vars));
        if !has_content_type {
            rb = rb.header(CONTENT_TYPE, "application/json");
        }
    }

    let start = Instant::now();
    let resp = rb.send().await.context("request failed")?;
    let elapsed = start.elapsed();

    let status = resp.status();
    let reason = status.canonical_reason().unwrap_or("").to_string();
    let is_json = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.contains("json"))
        .unwrap_or(false);
    let resp_headers: Vec<(String, String)> = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let bytes = resp.bytes().await.context("reading response body")?;
    let size = bytes.len();
    let raw = String::from_utf8_lossy(&bytes).into_owned();
    let body = if is_json {
        serde_json::from_str::<serde_json::Value>(&raw)
            .and_then(|v| serde_json::to_string_pretty(&v))
            .unwrap_or(raw)
    } else {
        raw
    };

    Ok(HttpResponse {
        status: status.as_u16(),
        reason,
        elapsed,
        headers: resp_headers,
        body,
        size,
    })
}

/// Build the final URL: base + path with `{{vars}}` and `{pathParams}` applied.
pub fn build_url(base_url: &str, req: &SavedRequest, vars: &HashMap<String, String>) -> String {
    let mut path = substitute(&req.path, vars);
    for row in req.path_params.iter().filter(|r| r.enabled) {
        let value = encode_path_segment(&substitute(&row.value, vars));
        path = path.replace(&format!("{{{}}}", row.key), &value);
    }
    format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

/// Percent-encode a path parameter value (unreserved chars kept as-is).
fn encode_path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
