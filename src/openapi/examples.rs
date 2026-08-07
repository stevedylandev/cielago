//! Example payload generation from JSON schemas (OpenAPI 3.0/3.1 subset).

use serde_json::{Map, Value};

use super::resolve::deref;

/// Depth cap for generated structures (also breaks schema cycles).
pub const MAX_GEN_DEPTH: usize = 6;

/// Produce an example value for a schema, preferring explicitly authored
/// examples/defaults, then enums, then combinators, then type-based stubs.
pub fn example_for_schema(doc: &Value, schema: &Value) -> Value {
    gen_value(doc, schema, 0)
}

fn gen_value(doc: &Value, schema: &Value, depth: usize) -> Value {
    if depth > MAX_GEN_DEPTH {
        return Value::Null;
    }
    let schema = deref(doc, schema);

    if let Some(ex) = schema.get("example") {
        return ex.clone();
    }
    if let Some(def) = schema.get("default") {
        return def.clone();
    }
    // OpenAPI 3.1 / JSON Schema style `examples` array.
    if let Some(first) = schema
        .get("examples")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
    {
        return first.clone();
    }
    if let Some(first) = schema
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
    {
        return first.clone();
    }

    if let Some(all) = schema.get("allOf").and_then(Value::as_array) {
        let mut merged = Map::new();
        for sub in all {
            if let Value::Object(props) = gen_value(doc, sub, depth + 1) {
                for (k, v) in props {
                    merged.insert(k, v);
                }
            }
        }
        return Value::Object(merged);
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(first) = schema
            .get(key)
            .and_then(Value::as_array)
            .and_then(|a| a.first())
        {
            return gen_value(doc, first, depth + 1);
        }
    }

    let ty = schema.get("type").and_then(Value::as_str);
    // OpenAPI 3.1 allows `type` arrays like ["string", "null"].
    let ty = ty.or_else(|| {
        schema
            .get("type")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(Value::as_str)
    });
    let ty = ty.or_else(|| {
        if schema.get("properties").is_some() {
            Some("object")
        } else {
            None
        }
    });

    match ty {
        Some("object") => {
            let mut map = Map::new();
            if let Some(props) = schema.get("properties").and_then(Value::as_object) {
                for (k, sub) in props {
                    map.insert(k.clone(), gen_value(doc, sub, depth + 1));
                }
            }
            Value::Object(map)
        }
        Some("array") => {
            let item = schema
                .get("items")
                .map(|s| gen_value(doc, s, depth + 1))
                .unwrap_or(Value::Null);
            Value::Array(vec![item])
        }
        Some("integer") => Value::from(1),
        Some("number") => Value::from(1.0),
        Some("boolean") => Value::from(true),
        _ => string_stub(schema),
    }
}

fn string_stub(schema: &Value) -> Value {
    match schema.get("format").and_then(Value::as_str) {
        // Substituted with a fresh UUID v4 at send time.
        Some("uuid") => Value::from("{{uuid}}"),
        Some("date-time") => Value::from("2024-01-01T00:00:00Z"),
        Some("date") => Value::from("2024-01-01"),
        Some("email") => Value::from("user@example.com"),
        _ => Value::from("string"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prefers_authored_example() {
        let doc = json!({});
        let schema = json!({"type": "integer", "example": 42});
        assert_eq!(example_for_schema(&doc, &schema), json!(42));
    }

    #[test]
    fn generates_object_with_refs() {
        let doc = json!({
            "components": { "schemas": {
                "Pet": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "integer", "format": "int64"},
                        "name": {"type": "string"},
                        "tag": {"type": "string", "default": "friendly"}
                    }
                }
            }}
        });
        let schema = json!({"$ref": "#/components/schemas/Pet"});
        let v = example_for_schema(&doc, &schema);
        assert_eq!(v, json!({"id": 1, "name": "string", "tag": "friendly"}));
    }

    #[test]
    fn uuid_format_becomes_variable() {
        let doc = json!({});
        let schema = json!({"type": "string", "format": "uuid"});
        assert_eq!(example_for_schema(&doc, &schema), json!("{{uuid}}"));
    }

    #[test]
    fn all_of_merges() {
        let doc = json!({});
        let schema = json!({"allOf": [
            {"type": "object", "properties": {"a": {"type": "integer"}}},
            {"type": "object", "properties": {"b": {"type": "boolean"}}}
        ]});
        assert_eq!(
            example_for_schema(&doc, &schema),
            json!({"a": 1, "b": true})
        );
    }

    #[test]
    fn terminates_on_self_reference() {
        let doc = json!({
            "components": { "schemas": {
                "Node": {"type": "object", "properties": {
                    "child": {"$ref": "#/components/schemas/Node"}
                }}
            }}
        });
        let schema = json!({"$ref": "#/components/schemas/Node"});
        let _ = example_for_schema(&doc, &schema); // must terminate
    }
}
