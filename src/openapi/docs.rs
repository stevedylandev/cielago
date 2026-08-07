//! Turning schemas into the [`FieldDoc`]s the Docs tab renders: what type a
//! parameter or body field is, whether it's required, and which values it
//! accepts.
//!
//! This is a summary, not a spec viewer — the aim is answering "what can I put
//! here?" without leaving the terminal.

use std::collections::HashSet;

use serde_json::Value;

use super::resolve::deref;
use crate::model::FieldDoc;

/// Nesting cap when flattening a body schema. Also what terminates recursive
/// schemas, the same way [`super::examples`] caps generation depth.
const MAX_BODY_DEPTH: usize = 4;

/// Upper bound on body fields per request, so a sprawling schema can't turn
/// the Docs tab into thousands of lines.
const MAX_BODY_FIELDS: usize = 200;

/// Documentation for one OpenAPI parameter object.
pub fn param_doc(doc: &Value, p: &Value) -> FieldDoc {
    let location = p.get("in").and_then(Value::as_str).unwrap_or("query");
    let schema = p.get("schema").map(|s| deref(doc, s));
    let mut field = match schema {
        Some(schema) => field_doc(doc, schema),
        None => FieldDoc {
            ty: "string".into(),
            ..FieldDoc::default()
        },
    };
    field.name = p
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    field.location = location.to_string();
    // Path parameters are required by definition (OpenAPI says so even when
    // the spec omits the flag).
    field.required =
        location == "path" || p.get("required").and_then(Value::as_bool).unwrap_or(false);
    // A description on the parameter beats one inherited from its schema.
    if let Some(d) = description(p) {
        field.description = Some(d);
    }
    field
}

/// Documentation for a request body schema, flattened to dotted paths:
/// `owner.name`, `pets[].tag`. A body that isn't an object gets a single row.
pub fn body_docs(doc: &Value, schema: &Value) -> Vec<FieldDoc> {
    let mut out = Vec::new();
    flatten(doc, schema, "", 0, &mut out);
    if out.is_empty() {
        let mut field = field_doc(doc, schema);
        if !field.ty.is_empty() && field.ty != "object" {
            field.name = "(body)".into();
            field.location = "body".into();
            out.push(field);
        }
    }
    out
}

fn flatten(doc: &Value, schema: &Value, prefix: &str, depth: usize, out: &mut Vec<FieldDoc>) {
    if depth > MAX_BODY_DEPTH || out.len() >= MAX_BODY_FIELDS {
        return;
    }
    let schema = deref(doc, schema);

    // An array contributes no fields of its own; describe its items under
    // `name[]` so the path reads like the JSON it documents.
    if let Some(items) = schema.get("items") {
        flatten(doc, items, &format!("{prefix}[]"), depth + 1, out);
        return;
    }

    for part in object_parts(doc, schema) {
        let required: HashSet<&str> = part
            .get("required")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let Some(props) = part.get("properties").and_then(Value::as_object) else {
            continue;
        };
        for (name, sub) in props {
            if out.len() >= MAX_BODY_FIELDS {
                return;
            }
            let sub = deref(doc, sub);
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}.{name}")
            };
            let mut field = field_doc(doc, sub);
            field.name = path.clone();
            field.location = "body".into();
            field.required = required.contains(name.as_str());
            out.push(field);
            // Scalars fall straight back out of this call.
            flatten(doc, sub, &path, depth + 1, out);
        }
    }
}

/// Schemas contributing properties to `schema`: itself, plus `allOf` members,
/// which OpenAPI uses for composition/inheritance.
fn object_parts<'a>(doc: &'a Value, schema: &'a Value) -> Vec<&'a Value> {
    let mut parts = vec![schema];
    if let Some(all) = schema.get("allOf").and_then(Value::as_array) {
        parts.extend(all.iter().map(|s| deref(doc, s)));
    }
    parts
}

/// Everything about a schema except the name and location, which only the
/// caller knows.
fn field_doc(doc: &Value, schema: &Value) -> FieldDoc {
    let schema = deref(doc, schema);
    FieldDoc {
        name: String::new(),
        location: String::new(),
        ty: type_label(doc, schema, 0),
        required: false,
        options: enum_options(doc, schema),
        description: description(schema),
        default: schema.get("default").map(scalar),
    }
}

/// A short, readable type: `string(uuid)`, `array<integer>`, `object`.
fn type_label(doc: &Value, schema: &Value, depth: usize) -> String {
    if depth > MAX_BODY_DEPTH {
        return "…".into();
    }
    let schema = deref(doc, schema);

    for key in ["oneOf", "anyOf"] {
        if let Some(alts) = schema.get(key).and_then(Value::as_array) {
            let labels: Vec<String> = alts
                .iter()
                .take(3)
                .map(|s| type_label(doc, s, depth + 1))
                .collect();
            let more = if alts.len() > 3 { " | …" } else { "" };
            return format!("{}{more}", labels.join(" | "));
        }
    }
    if schema.get("allOf").is_some() {
        return "object".into();
    }

    let types = type_names(schema);
    let Some(primary) = types.first() else {
        return if schema.get("properties").is_some() {
            "object".into()
        } else {
            "any".into()
        };
    };

    let mut label = match primary.as_str() {
        "array" => {
            let inner = schema
                .get("items")
                .map(|i| type_label(doc, i, depth + 1))
                .unwrap_or_else(|| "any".into());
            format!("array<{inner}>")
        }
        other => match schema.get("format").and_then(Value::as_str) {
            Some(f) => format!("{other}({f})"),
            None => other.to_string(),
        },
    };
    // OpenAPI 3.1 `type: [string, "null"]`.
    for extra in types.iter().skip(1) {
        label.push_str(" | ");
        label.push_str(extra);
    }
    label
}

/// `type` as a list — a plain string in 3.0, possibly an array in 3.1.
fn type_names(schema: &Value) -> Vec<String> {
    match schema.get("type") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect(),
        _ => Vec::new(),
    }
}

/// Accepted values: the schema's own `enum`, or an array's item `enum` (the
/// options are what goes *in* the array either way).
fn enum_options(doc: &Value, schema: &Value) -> Vec<String> {
    let direct = schema.get("enum").and_then(Value::as_array);
    let from_items = || {
        deref(doc, schema.get("items")?)
            .get("enum")
            .and_then(Value::as_array)
    };
    direct
        .or_else(from_items)
        .map(|a| a.iter().map(scalar).collect())
        .unwrap_or_default()
}

fn description(v: &Value) -> Option<String> {
    let d = v.get("description").and_then(Value::as_str)?.trim();
    (!d.is_empty()).then(|| d.to_string())
}

/// Enum entries and defaults are shown as they'd be typed into a field, so
/// strings lose their quotes.
fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parameter_types_enums_and_requiredness() {
        let doc = json!({});
        let p = json!({
            "name": "status",
            "in": "query",
            "required": true,
            "description": "Status values to filter by",
            "schema": {"type": "string", "enum": ["available", "pending", "sold"], "default": "available"}
        });
        let d = param_doc(&doc, &p);
        assert_eq!(d.name, "status");
        assert_eq!(d.location, "query");
        assert_eq!(d.ty, "string");
        assert!(d.required);
        assert_eq!(d.options, ["available", "pending", "sold"]);
        assert_eq!(d.default.as_deref(), Some("available"));
        assert_eq!(d.description.as_deref(), Some("Status values to filter by"));
    }

    #[test]
    fn path_params_are_required_even_when_unflagged() {
        let doc = json!({});
        let p = json!({"name": "petId", "in": "path", "schema": {"type": "integer", "format": "int64"}});
        let d = param_doc(&doc, &p);
        assert!(d.required);
        assert_eq!(d.ty, "integer(int64)");
    }

    #[test]
    fn array_params_expose_item_options() {
        let doc = json!({});
        let p = json!({
            "name": "tags",
            "in": "query",
            "schema": {"type": "array", "items": {"type": "string", "enum": ["a", "b"]}}
        });
        let d = param_doc(&doc, &p);
        assert_eq!(d.ty, "array<string>");
        assert_eq!(d.options, ["a", "b"]);
    }

    #[test]
    fn body_is_flattened_to_dotted_paths() {
        let doc = json!({
            "components": {"schemas": {
                "Address": {"type": "object", "required": ["zip"], "properties": {
                    "zip": {"type": "string"}
                }}
            }}
        });
        let schema = json!({
            "type": "object",
            "required": ["name"],
            "properties": {
                "name": {"type": "string"},
                "owner": {"type": "object", "properties": {
                    "address": {"$ref": "#/components/schemas/Address"}
                }},
                "pets": {"type": "array", "items": {"type": "object", "properties": {
                    "tag": {"type": "string", "enum": ["cat", "dog"]}
                }}}
            }
        });
        let docs = body_docs(&doc, &schema);
        let names: Vec<&str> = docs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "name",
                "owner",
                "owner.address",
                "owner.address.zip",
                "pets",
                "pets[].tag"
            ]
        );
        assert!(docs[0].required);
        assert!(!docs[1].required);
        let zip = docs.iter().find(|d| d.name == "owner.address.zip").unwrap();
        assert!(zip.required, "requiredness comes from the owning object");
        let tag = docs.iter().find(|d| d.name == "pets[].tag").unwrap();
        assert_eq!(tag.options, ["cat", "dog"]);
        assert_eq!(
            docs.iter().find(|d| d.name == "pets").unwrap().ty,
            "array<object>"
        );
        assert!(docs.iter().all(|d| d.location == "body"));
    }

    #[test]
    fn all_of_members_contribute_fields() {
        let doc = json!({});
        let schema = json!({"allOf": [
            {"type": "object", "required": ["id"], "properties": {"id": {"type": "integer"}}},
            {"type": "object", "properties": {"note": {"type": "string"}}}
        ]});
        let docs = body_docs(&doc, &schema);
        let names: Vec<&str> = docs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["id", "note"]);
        assert!(docs[0].required);
    }

    #[test]
    fn non_object_body_gets_one_row() {
        let doc = json!({});
        let docs = body_docs(&doc, &json!({"type": "string", "format": "binary"}));
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].name, "(body)");
        assert_eq!(docs[0].ty, "string(binary)");
    }

    #[test]
    fn recursive_schemas_terminate() {
        let doc = json!({
            "components": {"schemas": {
                "Node": {"type": "object", "properties": {
                    "child": {"$ref": "#/components/schemas/Node"}
                }}
            }}
        });
        let docs = body_docs(&doc, &json!({"$ref": "#/components/schemas/Node"}));
        assert!(!docs.is_empty());
        assert!(docs.len() <= MAX_BODY_DEPTH + 1, "{}", docs.len());
    }

    #[test]
    fn union_and_nullable_types_read_as_written() {
        let doc = json!({});
        assert_eq!(
            type_label(&doc, &json!({"type": ["string", "null"]}), 0),
            "string | null"
        );
        assert_eq!(
            type_label(
                &doc,
                &json!({"oneOf": [{"type": "string"}, {"type": "integer"}]}),
                0
            ),
            "string | integer"
        );
        assert_eq!(type_label(&doc, &json!({}), 0), "any");
    }
}
