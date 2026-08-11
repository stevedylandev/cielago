//! Conversion of a parsed OpenAPI document into a cielago [`Collection`].

use std::collections::HashSet;

use serde_json::Value;

use super::docs::{body_docs, param_doc, response_docs};
use super::examples::example_for_schema;
use super::resolve::deref;
use crate::model::{AuthStyle, Collection, KeyValueRow, Method, OAuthConfig, SavedRequest};

const METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];

pub fn import_spec(doc: &Value, name: &str, source: Option<String>) -> Collection {
    let mut collection = Collection::new(name);
    collection.spec_source = source;

    if let Some(servers) = doc.get("servers").and_then(Value::as_array) {
        for s in servers {
            if let Some(url) = s.get("url").and_then(Value::as_str) {
                let url = url.trim_end_matches('/').to_string();
                if !url.is_empty() && !collection.servers.contains(&url) {
                    collection.servers.push(url);
                }
            }
        }
    }

    collection.auth = extract_oauth(doc);

    if let Some(paths) = doc.get("paths").and_then(Value::as_object) {
        for (path, item) in paths {
            let item = deref(doc, item);
            let path_level_params = item.get("parameters").and_then(Value::as_array);
            for method in METHODS {
                let Some(op) = item.get(method) else { continue };
                collection
                    .requests
                    .push(build_request(doc, path, method, path_level_params, op));
            }
        }
    }

    collection
}

/// Find the first `oauth2` security scheme with a clientCredentials flow and
/// prefill token URL + scopes (credentials are filled in by the user).
fn extract_oauth(doc: &Value) -> Option<OAuthConfig> {
    let schemes = doc.get("components")?.get("securitySchemes")?.as_object()?;
    for (_, scheme) in schemes {
        let scheme = deref(doc, scheme);
        if scheme.get("type").and_then(Value::as_str) != Some("oauth2") {
            continue;
        }
        let Some(flow) = scheme.get("flows").and_then(|f| f.get("clientCredentials")) else {
            continue;
        };
        let token_url = flow
            .get("tokenUrl")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let scopes = flow
            .get("scopes")
            .and_then(Value::as_object)
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        return Some(OAuthConfig {
            token_url,
            scopes,
            auth_style: AuthStyle::Basic,
            ..Default::default()
        });
    }
    None
}

fn build_request(
    doc: &Value,
    path: &str,
    method: &str,
    path_level_params: Option<&Vec<Value>>,
    op: &Value,
) -> SavedRequest {
    let summary = op
        .get("summary")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string);
    let operation_id = op
        .get("operationId")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string);

    // `summary` first: it's the human-readable descriptor. `operationId` is
    // often a long generated controller name.
    let name = summary
        .clone()
        .or_else(|| operation_id.clone())
        .unwrap_or_else(|| format!("{} {}", method.to_uppercase(), path));

    let mut req = SavedRequest::blank(name);
    req.summary = summary;
    req.operation_id = operation_id;
    req.description = op
        .get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    req.method = Method::parse(method).unwrap_or(Method::Get);
    req.path = path.to_string();
    req.tags = op
        .get("tags")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    // Merge operation-level and path-level parameters; operation wins on
    // duplicate (name, in) pairs.
    let mut merged: Vec<&Value> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();

    let op_params = op.get("parameters").and_then(Value::as_array);
    for p in op_params.into_iter().flatten() {
        let p = deref(doc, p);
        let key = param_key(p);
        seen.insert(key);
        merged.push(p);
    }
    for p in path_level_params.into_iter().flatten() {
        let p = deref(doc, p);
        if seen.insert(param_key(p)) {
            merged.push(p);
        }
    }

    for p in merged {
        let name = p.get("name").and_then(Value::as_str).unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let required = p.get("required").and_then(Value::as_bool).unwrap_or(false);
        let location = p.get("in").and_then(Value::as_str).unwrap_or("");
        let value = param_value(doc, p, required);
        match location {
            "path" => req.path_params.push(KeyValueRow::new(name, value, true)),
            "query" => req.query.push(KeyValueRow::new(name, value, required)),
            "header" => req.headers.push(KeyValueRow::new(name, value, required)),
            _ => continue, // cookies etc. unsupported in v1
        }
        req.docs.push(param_doc(doc, p));
    }

    // Headers the spec implies without listing them as `in: header` params.
    // Explicit params win on a name collision.
    for row in implied_headers(doc, op) {
        if !req
            .headers
            .iter()
            .any(|h| h.key.eq_ignore_ascii_case(&row.key))
        {
            req.headers.push(row);
        }
    }

    req.body = extract_body(doc, op);
    if let Some(schema) = body_media(doc, op).and_then(|m| m.get("schema")) {
        req.docs.extend(body_docs(doc, schema));
    }
    if let Some(content) = success_response_content(doc, op)
        && let Some((_, media)) = pick_media(content)
        && let Some(schema) = media.get("schema")
    {
        req.docs.extend(response_docs(doc, schema));
    }
    req
}

/// Headers an operation carries by definition rather than by parameter: the
/// media type it consumes, the one it produces, and any apiKey-in-header
/// security scheme it requires. API-key rows arrive disabled — the value is
/// the user's to supply.
fn implied_headers(doc: &Value, op: &Value) -> Vec<KeyValueRow> {
    let mut out = Vec::new();
    if let Some(ct) = request_media_type(doc, op) {
        out.push(KeyValueRow::new("Content-Type", ct, true));
    }
    if let Some(accept) = response_media_type(doc, op) {
        out.push(KeyValueRow::new("Accept", accept, true));
    }
    for name in api_key_headers(doc, op) {
        out.push(KeyValueRow::new(name, "", false));
    }
    out
}

fn request_media_type(doc: &Value, op: &Value) -> Option<String> {
    let rb = deref(doc, op.get("requestBody")?);
    let content = rb.get("content").and_then(Value::as_object)?;
    pick_media(content).map(|(k, _)| k.clone())
}

/// Media type from the first success response (or `default`), so `Accept`
/// matches what the endpoint actually returns.
fn response_media_type(doc: &Value, op: &Value) -> Option<String> {
    let content = success_response_content(doc, op)?;
    pick_media(content).map(|(k, _)| k.clone())
}

/// The `content` map of the first success response (or `default`) — the shape
/// the endpoint returns, used for both the `Accept` header and response docs.
fn success_response_content<'a>(
    doc: &'a Value,
    op: &'a Value,
) -> Option<&'a serde_json::Map<String, Value>> {
    let responses = op.get("responses").and_then(Value::as_object)?;
    let resp = responses
        .iter()
        .find(|(code, _)| code.starts_with('2'))
        .or_else(|| {
            responses
                .iter()
                .find(|(code, _)| code.as_str() == "default")
        })
        .map(|(_, v)| v)?;
    deref(doc, resp).get("content").and_then(Value::as_object)
}

/// Header names from apiKey security schemes this operation requires,
/// preferring operation-level `security` over the document default.
fn api_key_headers(doc: &Value, op: &Value) -> Vec<String> {
    let Some(requirements) = op
        .get("security")
        .or_else(|| doc.get("security"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let Some(schemes) = doc
        .get("components")
        .and_then(|c| c.get("securitySchemes"))
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };

    let mut out: Vec<String> = Vec::new();
    for requirement in requirements {
        let Some(obj) = requirement.as_object() else {
            continue;
        };
        for scheme_name in obj.keys() {
            let Some(scheme) = schemes.get(scheme_name) else {
                continue;
            };
            let scheme = deref(doc, scheme);
            if scheme.get("type").and_then(Value::as_str) != Some("apiKey")
                || scheme.get("in").and_then(Value::as_str) != Some("header")
            {
                continue;
            }
            if let Some(name) = scheme.get("name").and_then(Value::as_str)
                && !name.is_empty()
                && !out.iter().any(|e| e.eq_ignore_ascii_case(name))
            {
                out.push(name.to_string());
            }
        }
    }
    out
}

fn param_key(p: &Value) -> (String, String) {
    (
        p.get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        p.get("in")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
    )
}

/// Value for a parameter. Required params fall back to type-based stubs so the
/// request is sendable out of the box; optional params only get explicitly
/// authored examples/defaults (otherwise empty).
fn param_value(doc: &Value, p: &Value, required: bool) -> String {
    if let Some(v) = explicit_param_value(doc, p) {
        return v;
    }
    if !required {
        return String::new();
    }
    match p.get("schema") {
        Some(schema) => value_to_string(&example_for_schema(doc, deref(doc, schema))),
        None => String::new(),
    }
}

/// Explicitly authored example/default on the parameter or its schema.
fn explicit_param_value(doc: &Value, p: &Value) -> Option<String> {
    if let Some(ex) = p.get("example") {
        return Some(value_to_string(ex));
    }
    if let Some(exs) = p.get("examples").and_then(Value::as_object)
        && let Some((_, first)) = exs.iter().next()
    {
        let first = deref(doc, first);
        if let Some(v) = first.get("value") {
            return Some(value_to_string(v));
        }
    }
    let schema = deref(doc, p.get("schema")?);
    if let Some(ex) = schema.get("example") {
        return Some(value_to_string(ex));
    }
    if let Some(def) = schema.get("default") {
        return Some(value_to_string(def));
    }
    None
}

/// Request body from `requestBody`, preferring JSON media types; falls back to
/// a schema-generated example so payloads are always populated and editable.
fn extract_body(doc: &Value, op: &Value) -> Option<String> {
    let media = body_media(doc, op)?;

    if let Some(ex) = media.get("example") {
        return Some(body_to_string(ex));
    }
    if let Some(exs) = media.get("examples").and_then(Value::as_object)
        && let Some((_, first)) = exs.iter().next()
    {
        let first = deref(doc, first);
        if let Some(v) = first.get("value") {
            return Some(body_to_string(v));
        }
    }
    let schema = media.get("schema")?;
    Some(body_to_string(&example_for_schema(doc, schema)))
}

/// The media-type entry of `requestBody` that cielago sends — and therefore
/// the one both the generated body and the Docs tab describe.
fn body_media<'a>(doc: &'a Value, op: &'a Value) -> Option<&'a Value> {
    let rb = deref(doc, op.get("requestBody")?);
    let content = rb.get("content").and_then(Value::as_object)?;
    pick_media(content).map(|(_, media)| media)
}

/// Preferred entry from a `content` map: JSON first, then anything JSON-ish,
/// then whatever the spec listed first.
fn pick_media(content: &serde_json::Map<String, Value>) -> Option<(&String, &Value)> {
    content
        .iter()
        .find(|(k, _)| k.as_str() == "application/json")
        .or_else(|| content.iter().find(|(k, _)| k.contains("json")))
        .or_else(|| content.iter().next())
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn body_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}
