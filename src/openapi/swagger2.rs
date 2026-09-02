//! Swagger 2.0 → OpenAPI 3.0 normalisation.
//!
//! Rather than teach every downstream module two spec dialects, a 2.0 document
//! is rewritten into the 3.0 shape as it's parsed ([`super::loader::parse_spec`]
//! calls [`to_openapi3`]), so `import`, `examples` and `docs` only ever see
//! 3.x. The conversion is structural and lossy in the places 3.0 has no slot
//! for — see the module's `convert_*` docs.

use serde_json::{Map, Value};

use super::resolve::deref;

const METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];

/// Keys 2.0 puts directly on a non-body parameter (or a response header) that
/// 3.0 nests under `schema`.
const SCHEMA_KEYS: [&str; 16] = [
    "type",
    "format",
    "items",
    "default",
    "enum",
    "maximum",
    "exclusiveMaximum",
    "minimum",
    "exclusiveMinimum",
    "maxLength",
    "minLength",
    "pattern",
    "maxItems",
    "minItems",
    "uniqueItems",
    "multipleOf",
];

/// Where the 2.0 definition sections move to under `components`, and therefore
/// how `$ref`s into them have to be rewritten.
const REF_MOVES: [(&str, &str); 3] = [
    ("#/definitions/", "#/components/schemas/"),
    ("#/parameters/", "#/components/parameters/"),
    ("#/responses/", "#/components/responses/"),
];

const DEFAULT_MEDIA: &str = "application/json";
const FORM_MEDIA: &str = "application/x-www-form-urlencoded";
const MULTIPART_MEDIA: &str = "multipart/form-data";

/// Is this a Swagger 2.0 document? 3.x documents carry `openapi` instead.
pub fn is_swagger2(doc: &Value) -> bool {
    doc.get("swagger")
        .and_then(Value::as_str)
        .is_some_and(|v| v.starts_with("2."))
}

/// Rewrite a Swagger 2.0 document into an equivalent OpenAPI 3.0 one.
/// Anything the conversion doesn't recognise is carried across untouched, so
/// vendor extensions and unknown fields survive.
pub fn to_openapi3(doc: Value) -> Value {
    let mut doc = doc;
    rewrite_refs(&mut doc);
    let mut root = match doc {
        Value::Object(map) => map,
        other => return other,
    };

    root.remove("swagger");
    let consumes = string_list(root.remove("consumes").as_ref());
    let produces = string_list(root.remove("produces").as_ref());
    let servers = servers_from(&mut root);
    let components = build_components(&mut root, &produces);

    // Parameters are referenced by `$ref` from operations, and whether one is a
    // body parameter decides where it lands — so operation conversion needs to
    // resolve against the components built above.
    let mut refdoc = Map::new();
    refdoc.insert("components".into(), components.clone());
    let refdoc = Value::Object(refdoc);

    let paths = convert_paths(root.remove("paths"), &refdoc, &consumes, &produces);

    let mut out = Map::new();
    out.insert("openapi".into(), Value::from("3.0.0"));
    // Whatever is left (info, security, tags, externalDocs, x-…) keeps its 2.0
    // meaning in 3.0.
    for (key, value) in root {
        out.insert(key, value);
    }
    if !servers.is_empty() {
        out.insert("servers".into(), Value::Array(servers));
    }
    out.insert("paths".into(), paths);
    if components.as_object().is_some_and(|c| !c.is_empty()) {
        out.insert("components".into(), components);
    }
    Value::Object(out)
}

/// Point every local `$ref` at its new home under `components`.
fn rewrite_refs(node: &mut Value) {
    match node {
        Value::Object(map) => {
            if let Some(Value::String(reference)) = map.get_mut("$ref") {
                for (from, to) in REF_MOVES {
                    if let Some(rest) = reference.strip_prefix(from) {
                        *reference = format!("{to}{rest}");
                        break;
                    }
                }
            }
            for (_, child) in map.iter_mut() {
                rewrite_refs(child);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(rewrite_refs),
        _ => {}
    }
}

/// `schemes` × `host` + `basePath` → `servers`. `https` is ordered first when a
/// spec offers both, since that's the better default active server; non-HTTP
/// schemes (`ws`, `wss`) are dropped. A spec with only a `basePath` yields the
/// relative URL it implies, which the user can replace at import time.
fn servers_from(root: &mut Map<String, Value>) -> Vec<Value> {
    let host = take_str(root, "host");
    let base = take_str(root, "basePath");
    let base = base.trim_end_matches('/').to_string();

    if host.is_empty() {
        return if base.is_empty() {
            Vec::new()
        } else {
            vec![server(&base)]
        };
    }

    let mut schemes = string_list(root.remove("schemes").as_ref());
    schemes.retain(|s| s == "http" || s == "https");
    schemes.sort_by_key(|s| usize::from(s != "https"));
    schemes.dedup();
    if schemes.is_empty() {
        schemes.push("https".into());
    }
    schemes
        .iter()
        .map(|scheme| server(&format!("{scheme}://{host}{base}")))
        .collect()
}

fn server(url: &str) -> Value {
    let mut map = Map::new();
    map.insert("url".into(), Value::from(url));
    Value::Object(map)
}

/// Move the 2.0 top-level definition sections under `components`, converting
/// each entry to its 3.0 shape. Shared body/formData parameters are left in 2.0
/// form: they have no 3.0 counterpart, and [`split_params`] lifts them into a
/// `requestBody` wherever they're referenced.
fn build_components(root: &mut Map<String, Value>, produces: &[String]) -> Value {
    let mut components = Map::new();

    if let Some(definitions) = root.remove("definitions") {
        components.insert("schemas".into(), definitions);
    }
    if let Some(Value::Object(params)) = root.remove("parameters") {
        let converted = params
            .into_iter()
            .map(|(name, p)| {
                let value = if is_payload_param(&p) {
                    p
                } else {
                    convert_param(p)
                };
                (name, value)
            })
            .collect();
        components.insert("parameters".into(), Value::Object(converted));
    }
    if let Some(Value::Object(responses)) = root.remove("responses") {
        let converted = responses
            .into_iter()
            .map(|(code, r)| (code, convert_response(r, produces)))
            .collect();
        components.insert("responses".into(), Value::Object(converted));
    }
    if let Some(Value::Object(schemes)) = root.remove("securityDefinitions") {
        let converted = schemes
            .into_iter()
            .map(|(name, s)| (name, convert_security_scheme(s)))
            .collect();
        components.insert("securitySchemes".into(), Value::Object(converted));
    }

    Value::Object(components)
}

/// 2.0 security definitions → 3.0 security schemes. `basic` becomes HTTP basic;
/// oauth2's single `flow` becomes the matching entry in `flows`. `apiKey` is
/// already the 3.0 shape.
fn convert_security_scheme(scheme: Value) -> Value {
    let mut scheme = match scheme {
        Value::Object(map) => map,
        other => return other,
    };
    match scheme.get("type").and_then(Value::as_str) {
        Some("basic") => {
            scheme.insert("type".into(), Value::from("http"));
            scheme.insert("scheme".into(), Value::from("basic"));
        }
        Some("oauth2") => {
            let flow_name = match take_str(&mut scheme, "flow").as_str() {
                "application" => "clientCredentials",
                "accessCode" => "authorizationCode",
                "password" => "password",
                _ => "implicit",
            };
            let mut flow = Map::new();
            for key in ["authorizationUrl", "tokenUrl", "scopes"] {
                if let Some(v) = scheme.remove(key) {
                    flow.insert(key.into(), v);
                }
            }
            flow.entry("scopes")
                .or_insert_with(|| Value::Object(Map::new()));
            let mut flows = Map::new();
            flows.insert(flow_name.into(), Value::Object(flow));
            scheme.insert("flows".into(), Value::Object(flows));
        }
        _ => {}
    }
    Value::Object(scheme)
}

fn convert_paths(
    paths: Option<Value>,
    refdoc: &Value,
    consumes: &[String],
    produces: &[String],
) -> Value {
    let Some(Value::Object(paths)) = paths else {
        return Value::Object(Map::new());
    };
    let mut out = Map::new();
    for (path, item) in paths {
        let mut item = match item {
            Value::Object(map) => map,
            other => {
                out.insert(path, other);
                continue;
            }
        };
        // A path-level body parameter applies to every operation under it.
        let (shared_params, shared_body) =
            split_params(item.remove("parameters"), refdoc, consumes);
        if !shared_params.is_empty() {
            item.insert("parameters".into(), Value::Array(shared_params));
        }
        for method in METHODS {
            if let Some(op) = item.remove(method) {
                let op = convert_operation(op, refdoc, consumes, produces, shared_body.as_ref());
                item.insert(method.into(), op);
            }
        }
        out.insert(path, Value::Object(item));
    }
    Value::Object(out)
}

/// Operation-level `consumes`/`produces` override the document's, and are the
/// media types the lifted `requestBody` and the converted responses are keyed
/// by. Operation-level `schemes` is dropped: 3.0 has no per-operation server.
fn convert_operation(
    op: Value,
    refdoc: &Value,
    consumes: &[String],
    produces: &[String],
    shared_body: Option<&Value>,
) -> Value {
    let mut op = match op {
        Value::Object(map) => map,
        other => return other,
    };

    let consumes = override_list(op.remove("consumes").as_ref(), consumes);
    let produces = override_list(op.remove("produces").as_ref(), produces);
    op.remove("schemes");

    let (params, body) = split_params(op.remove("parameters"), refdoc, &consumes);
    if !params.is_empty() {
        op.insert("parameters".into(), Value::Array(params));
    }
    if let Some(body) = body.or_else(|| shared_body.cloned()) {
        op.insert("requestBody".into(), body);
    }
    if let Some(responses) = op.remove("responses") {
        op.insert("responses".into(), convert_responses(responses, &produces));
    }

    Value::Object(op)
}

/// Split a 2.0 parameter list into the parameters 3.0 still calls parameters
/// and the `requestBody` the rest of them describe. `$ref`s are resolved only
/// far enough to tell which side an entry belongs on: a reference to a
/// query/header/path parameter is left as a reference.
fn split_params(
    params: Option<Value>,
    refdoc: &Value,
    consumes: &[String],
) -> (Vec<Value>, Option<Value>) {
    let Some(Value::Array(params)) = params else {
        return (Vec::new(), None);
    };

    let mut kept = Vec::new();
    let mut form = Vec::new();
    let mut body = None;
    for p in params {
        let resolved = deref(refdoc, &p);
        match resolved.get("in").and_then(Value::as_str) {
            // Last body parameter wins; a spec with two is already invalid.
            Some("body") => body = Some(resolved.clone()),
            Some("formData") => form.push(resolved.clone()),
            _ => kept.push(convert_param(p)),
        }
    }

    let request_body = match body {
        Some(body) => Some(body_request_body(&body, consumes)),
        None if !form.is_empty() => Some(form_request_body(&form, consumes)),
        None => None,
    };
    (kept, request_body)
}

/// A non-body parameter: 2.0 spells its type inline, 3.0 wants a `schema`.
fn convert_param(p: Value) -> Value {
    let mut p = match p {
        Value::Object(map) => map,
        other => return other,
    };
    if p.contains_key("$ref") || p.contains_key("schema") {
        return Value::Object(p);
    }
    // `style`/`explode` are 3.0's answer to collectionFormat; cielago sends
    // array params as a single comma-joined value either way.
    p.remove("collectionFormat");
    if let Some(example) = p.remove("x-example")
        && !p.contains_key("example")
    {
        p.insert("example".into(), example);
    }
    if let Some(schema) = lift_schema_keys(&mut p) {
        p.insert("schema".into(), schema);
    }
    Value::Object(p)
}

/// `in: body` → `requestBody`, keyed by every media type the operation
/// consumes so [`super::import`] can pick the JSON one.
fn body_request_body(param: &Value, consumes: &[String]) -> Value {
    let schema = param
        .get("schema")
        .cloned()
        .unwrap_or(Value::Object(Map::new()));
    let mut content = Map::new();
    for media_type in media_types(consumes) {
        let mut media = Map::new();
        media.insert("schema".into(), schema.clone());
        content.insert(media_type, Value::Object(media));
    }

    let mut body = Map::new();
    if let Some(description) = param.get("description") {
        body.insert("description".into(), description.clone());
    }
    body.insert(
        "required".into(),
        Value::from(param.get("required").and_then(Value::as_bool) == Some(true)),
    );
    body.insert("content".into(), Value::Object(content));
    Value::Object(body)
}

/// `in: formData` parameters are one body between them: they become the
/// properties of a single object schema, the way 3.0 models a form.
fn form_request_body(params: &[Value], consumes: &[String]) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    let mut has_file = false;

    for p in params {
        let Value::Object(obj) = p else { continue };
        let Some(name) = obj.get("name").and_then(Value::as_str).map(String::from) else {
            continue;
        };
        if obj.get("type").and_then(Value::as_str) == Some("file") {
            has_file = true;
        }
        if obj.get("required").and_then(Value::as_bool) == Some(true) {
            required.push(Value::from(name.clone()));
        }
        let mut obj = obj.clone();
        let mut schema = match lift_schema_keys(&mut obj) {
            Some(Value::Object(schema)) => schema,
            _ => Map::new(),
        };
        if let Some(description) = obj.remove("description") {
            schema.entry("description").or_insert(description);
        }
        properties.insert(name, Value::Object(schema));
    }

    // A file upload has to be multipart; anything else defaults to urlencoded
    // unless the spec named a form media type itself.
    let wanted = if has_file { "multipart/" } else { "form" };
    let media_type = consumes
        .iter()
        .find(|c| c.contains(wanted))
        .cloned()
        .unwrap_or_else(|| {
            if has_file {
                MULTIPART_MEDIA
            } else {
                FORM_MEDIA
            }
            .to_string()
        });

    let mut schema = Map::new();
    schema.insert("type".into(), Value::from("object"));
    if !required.is_empty() {
        schema.insert("required".into(), Value::Array(required.clone()));
    }
    schema.insert("properties".into(), Value::Object(properties));

    let mut media = Map::new();
    media.insert("schema".into(), Value::Object(schema));
    let mut content = Map::new();
    content.insert(media_type, Value::Object(media));

    let mut body = Map::new();
    body.insert("required".into(), Value::from(!required.is_empty()));
    body.insert("content".into(), Value::Object(content));
    Value::Object(body)
}

fn convert_responses(responses: Value, produces: &[String]) -> Value {
    let Value::Object(responses) = responses else {
        return responses;
    };
    let converted = responses
        .into_iter()
        .map(|(code, r)| (code, convert_response(r, produces)))
        .collect();
    Value::Object(converted)
}

/// A 2.0 response carries `schema` (and per-media-type `examples`) directly;
/// 3.0 keys both by media type under `content`.
fn convert_response(response: Value, produces: &[String]) -> Value {
    let mut response = match response {
        Value::Object(map) => map,
        other => return other,
    };
    if let Some(Value::Object(headers)) = response.get_mut("headers") {
        for (_, header) in headers.iter_mut() {
            if let Value::Object(header) = header
                && let Some(schema) = lift_schema_keys(header)
            {
                header.insert("schema".into(), schema);
            }
        }
    }
    if response.contains_key("$ref") || response.contains_key("content") {
        return Value::Object(response);
    }

    let schema = response.remove("schema");
    let examples = response.remove("examples");
    if schema.is_none() && examples.is_none() {
        return Value::Object(response);
    }

    let mut types = media_types(produces);
    if let Some(Value::Object(examples)) = &examples {
        for media_type in examples.keys() {
            if !types.contains(media_type) {
                types.push(media_type.clone());
            }
        }
    }

    let mut content = Map::new();
    for media_type in types {
        let mut media = Map::new();
        if let Some(schema) = &schema {
            media.insert("schema".into(), schema.clone());
        }
        if let Some(example) = examples.as_ref().and_then(|e| e.get(media_type.as_str())) {
            media.insert("example".into(), example.clone());
        }
        if !media.is_empty() {
            content.insert(media_type, Value::Object(media));
        }
    }
    if !content.is_empty() {
        response.insert("content".into(), Value::Object(content));
    }
    Value::Object(response)
}

/// Pull the inline type keywords out of a parameter or header into a schema.
/// 2.0's `type: file` is 3.0's binary string.
fn lift_schema_keys(obj: &mut Map<String, Value>) -> Option<Value> {
    let mut schema = Map::new();
    for key in SCHEMA_KEYS {
        if let Some(v) = obj.remove(key) {
            schema.insert(key.into(), v);
        }
    }
    if schema.is_empty() {
        return None;
    }
    if schema.get("type").and_then(Value::as_str) == Some("file") {
        schema.insert("type".into(), Value::from("string"));
        schema.insert("format".into(), Value::from("binary"));
    }
    Some(Value::Object(schema))
}

fn is_payload_param(p: &Value) -> bool {
    matches!(
        p.get("in").and_then(Value::as_str),
        Some("body") | Some("formData")
    )
}

fn string_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// An operation-level list wins over the document-level one, but only when it
/// actually lists something.
fn override_list(local: Option<&Value>, inherited: &[String]) -> Vec<String> {
    let local = string_list(local);
    if local.is_empty() {
        inherited.to_vec()
    } else {
        local
    }
}

/// Media types to key a `content` map by, falling back to JSON for a spec that
/// declared none.
fn media_types(list: &[String]) -> Vec<String> {
    if list.is_empty() {
        vec![DEFAULT_MEDIA.to_string()]
    } else {
        list.to_vec()
    }
}

fn take_str(map: &mut Map<String, Value>, key: &str) -> String {
    map.remove(key)
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn convert(doc: Value) -> Value {
        assert!(is_swagger2(&doc));
        to_openapi3(doc)
    }

    #[test]
    fn detects_the_dialect() {
        assert!(is_swagger2(&json!({"swagger": "2.0"})));
        assert!(!is_swagger2(&json!({"openapi": "3.0.3"})));
        assert!(!is_swagger2(&json!({})));
    }

    #[test]
    fn host_base_path_and_schemes_become_servers() {
        let out = convert(json!({
            "swagger": "2.0",
            "host": "api.example.com",
            "basePath": "/v1/",
            "schemes": ["http", "https", "wss"]
        }));
        // https first, the trailing slash trimmed, non-HTTP schemes dropped.
        assert_eq!(
            out["servers"],
            json!([
                {"url": "https://api.example.com/v1"},
                {"url": "http://api.example.com/v1"}
            ])
        );
    }

    #[test]
    fn missing_scheme_defaults_to_https_and_missing_host_keeps_base_path() {
        let out = convert(json!({"swagger": "2.0", "host": "api.example.com"}));
        assert_eq!(out["servers"], json!([{"url": "https://api.example.com"}]));

        let out = convert(json!({"swagger": "2.0", "basePath": "/v1"}));
        assert_eq!(out["servers"], json!([{"url": "/v1"}]));

        let out = convert(json!({"swagger": "2.0"}));
        assert!(out.get("servers").is_none());
    }

    #[test]
    fn definitions_move_and_refs_follow_them() {
        let out = convert(json!({
            "swagger": "2.0",
            "definitions": {"Pet": {"type": "object", "properties": {
                "friend": {"$ref": "#/definitions/Pet"}
            }}},
            "paths": {"/pets": {"post": {
                "parameters": [{"name": "body", "in": "body", "schema": {"$ref": "#/definitions/Pet"}}],
                "responses": {}
            }}}
        }));
        assert_eq!(out["components"]["schemas"]["Pet"]["type"], "object");
        assert_eq!(
            out["components"]["schemas"]["Pet"]["properties"]["friend"]["$ref"],
            "#/components/schemas/Pet"
        );
        assert_eq!(
            out["paths"]["/pets"]["post"]["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/Pet"
        );
    }

    #[test]
    fn body_parameter_becomes_a_request_body() {
        let out = convert(json!({
            "swagger": "2.0",
            "consumes": ["application/xml"],
            "paths": {"/pets": {"post": {
                "parameters": [
                    {"name": "pet", "in": "body", "required": true,
                     "description": "the pet", "schema": {"type": "object"}},
                    {"name": "trace", "in": "header", "type": "string"}
                ],
                "responses": {}
            }}}
        }));
        let op = &out["paths"]["/pets"]["post"];
        assert_eq!(op["requestBody"]["required"], true);
        assert_eq!(op["requestBody"]["description"], "the pet");
        assert_eq!(
            op["requestBody"]["content"]["application/xml"]["schema"]["type"],
            "object"
        );
        // The body parameter left the parameter list; the header stayed.
        assert_eq!(op["parameters"].as_array().unwrap().len(), 1);
        assert_eq!(op["parameters"][0]["name"], "trace");
    }

    #[test]
    fn form_data_parameters_become_one_object_schema() {
        let out = convert(json!({
            "swagger": "2.0",
            "paths": {"/upload": {"post": {
                "parameters": [
                    {"name": "caption", "in": "formData", "required": true,
                     "type": "string", "description": "what it shows"},
                    {"name": "photo", "in": "formData", "type": "file"}
                ],
                "responses": {}
            }}}
        }));
        let body = &out["paths"]["/upload"]["post"]["requestBody"];
        // A file forces multipart even though the spec listed no `consumes`.
        let schema = &body["content"]["multipart/form-data"]["schema"];
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["caption"]));
        assert_eq!(schema["properties"]["caption"]["type"], "string");
        assert_eq!(
            schema["properties"]["caption"]["description"],
            "what it shows"
        );
        // 2.0's `file` type is 3.0's binary string.
        assert_eq!(schema["properties"]["photo"]["type"], "string");
        assert_eq!(schema["properties"]["photo"]["format"], "binary");
    }

    #[test]
    fn form_without_a_file_defaults_to_urlencoded() {
        let out = convert(json!({
            "swagger": "2.0",
            "paths": {"/login": {"post": {
                "parameters": [{"name": "user", "in": "formData", "type": "string"}],
                "responses": {}
            }}}
        }));
        let content = &out["paths"]["/login"]["post"]["requestBody"]["content"];
        assert!(content.get("application/x-www-form-urlencoded").is_some());
    }

    #[test]
    fn inline_parameter_types_move_under_schema() {
        let out = convert(json!({
            "swagger": "2.0",
            "parameters": {"PetId": {
                "name": "petId", "in": "path", "required": true,
                "type": "integer", "format": "int64", "x-example": 123
            }},
            "paths": {"/pets": {"get": {
                "parameters": [{
                    "name": "tags", "in": "query", "type": "array",
                    "collectionFormat": "csv",
                    "items": {"type": "string", "enum": ["cat", "dog"]}
                }],
                "responses": {}
            }}}
        }));
        let shared = &out["components"]["parameters"]["PetId"];
        assert_eq!(
            shared["schema"],
            json!({"type": "integer", "format": "int64"})
        );
        assert_eq!(shared["in"], "path");
        // `x-example` is 2.0's only way to give a non-body parameter an example.
        assert_eq!(shared["example"], 123);

        let p = &out["paths"]["/pets"]["get"]["parameters"][0];
        assert_eq!(p["schema"]["type"], "array");
        assert_eq!(p["schema"]["items"]["enum"], json!(["cat", "dog"]));
        assert!(p.get("collectionFormat").is_none());
        assert!(p.get("type").is_none());
    }

    #[test]
    fn path_level_body_applies_to_each_operation() {
        let out = convert(json!({
            "swagger": "2.0",
            "paths": {"/pets": {
                "parameters": [
                    {"name": "pet", "in": "body", "schema": {"type": "object"}},
                    {"name": "petId", "in": "path", "required": true, "type": "string"}
                ],
                "put": {"responses": {}},
                "post": {
                    "parameters": [{"name": "own", "in": "body", "schema": {"type": "string"}}],
                    "responses": {}
                }
            }}
        }));
        let item = &out["paths"]["/pets"];
        // The path-level parameter list keeps only what 3.0 calls a parameter.
        assert_eq!(item["parameters"].as_array().unwrap().len(), 1);
        assert_eq!(item["parameters"][0]["schema"]["type"], "string");
        assert_eq!(
            item["put"]["requestBody"]["content"]["application/json"]["schema"]["type"],
            "object"
        );
        // An operation's own body wins over the inherited one.
        assert_eq!(
            item["post"]["requestBody"]["content"]["application/json"]["schema"]["type"],
            "string"
        );
    }

    #[test]
    fn security_definitions_become_security_schemes() {
        let out = convert(json!({
            "swagger": "2.0",
            "securityDefinitions": {
                "oauth": {
                    "type": "oauth2",
                    "flow": "application",
                    "tokenUrl": "https://auth.example.com/token",
                    "scopes": {"read": "Read"}
                },
                "code": {
                    "type": "oauth2",
                    "flow": "accessCode",
                    "authorizationUrl": "https://auth.example.com/authorize",
                    "tokenUrl": "https://auth.example.com/token"
                },
                "basic": {"type": "basic"},
                "key": {"type": "apiKey", "name": "X-Api-Key", "in": "header"}
            }
        }));
        let schemes = &out["components"]["securitySchemes"];
        assert_eq!(
            schemes["oauth"]["flows"]["clientCredentials"]["tokenUrl"],
            "https://auth.example.com/token"
        );
        assert_eq!(
            schemes["oauth"]["flows"]["clientCredentials"]["scopes"]["read"],
            "Read"
        );
        assert!(schemes["oauth"].get("flow").is_none());
        assert_eq!(
            schemes["code"]["flows"]["authorizationCode"]["authorizationUrl"],
            "https://auth.example.com/authorize"
        );
        // A flow with no declared scopes still gets the map 3.0 requires.
        assert_eq!(
            schemes["code"]["flows"]["authorizationCode"]["scopes"],
            json!({})
        );
        assert_eq!(schemes["basic"], json!({"type": "http", "scheme": "basic"}));
        assert_eq!(
            schemes["key"],
            json!({"type": "apiKey", "name": "X-Api-Key", "in": "header"})
        );
    }

    #[test]
    fn response_schemas_and_examples_move_under_content() {
        let out = convert(json!({
            "swagger": "2.0",
            "produces": ["application/json"],
            "paths": {"/pets": {"get": {
                "responses": {
                    "200": {
                        "description": "ok",
                        "schema": {"type": "array", "items": {"type": "string"}},
                        "examples": {"application/json": ["fido"]}
                    },
                    "204": {"description": "empty"}
                }
            }}}
        }));
        let responses = &out["paths"]["/pets"]["get"]["responses"];
        let media = &responses["200"]["content"]["application/json"];
        assert_eq!(media["schema"]["type"], "array");
        assert_eq!(media["example"], json!(["fido"]));
        assert_eq!(responses["200"]["description"], "ok");
        // Nothing to describe means no `content` at all.
        assert!(responses["204"].get("content").is_none());
    }

    #[test]
    fn operation_media_types_override_the_document() {
        let out = convert(json!({
            "swagger": "2.0",
            "consumes": ["application/json"],
            "produces": ["application/json"],
            "paths": {"/pets": {"post": {
                "consumes": ["text/plain"],
                "produces": ["text/plain"],
                "parameters": [{"name": "b", "in": "body", "schema": {"type": "string"}}],
                "responses": {"200": {"description": "ok", "schema": {"type": "string"}}}
            }}}
        }));
        let op = &out["paths"]["/pets"]["post"];
        assert!(op["requestBody"]["content"].get("text/plain").is_some());
        assert!(
            op["requestBody"]["content"]
                .get("application/json")
                .is_none()
        );
        assert!(
            op["responses"]["200"]["content"]
                .get("text/plain")
                .is_some()
        );
        assert!(op.get("consumes").is_none());
    }

    #[test]
    fn response_headers_get_schemas() {
        let out = convert(json!({
            "swagger": "2.0",
            "paths": {"/pets": {"get": {"responses": {"200": {
                "description": "ok",
                "headers": {"X-Rate-Limit": {"type": "integer", "description": "calls left"}}
            }}}}}
        }));
        let header = &out["paths"]["/pets"]["get"]["responses"]["200"]["headers"]["X-Rate-Limit"];
        assert_eq!(header["schema"], json!({"type": "integer"}));
        assert_eq!(header["description"], "calls left");
    }

    #[test]
    fn unrecognised_fields_are_carried_across() {
        let out = convert(json!({
            "swagger": "2.0",
            "info": {"title": "t", "version": "1"},
            "tags": [{"name": "pets"}],
            "security": [{"key": []}],
            "x-logo": {"url": "https://example.com/logo.png"}
        }));
        assert_eq!(out["openapi"], "3.0.0");
        assert!(out.get("swagger").is_none());
        assert_eq!(out["info"]["title"], "t");
        assert_eq!(out["tags"][0]["name"], "pets");
        assert_eq!(out["security"], json!([{"key": []}]));
        assert_eq!(out["x-logo"]["url"], "https://example.com/logo.png");
    }
}
