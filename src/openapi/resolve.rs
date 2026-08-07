use serde_json::Value;

/// Safety cap on `$ref` chains to survive reference cycles.
pub const MAX_DEREF_DEPTH: usize = 16;

/// Resolve a local reference like `#/components/schemas/Pet` against the document.
/// Remote references are not supported and resolve to `None`.
pub fn resolve_pointer<'a>(doc: &'a Value, reference: &str) -> Option<&'a Value> {
    let ptr = reference.strip_prefix('#')?;
    if ptr.is_empty() {
        return Some(doc);
    }
    doc.pointer(ptr)
}

/// Follow `$ref` chains (with a depth cap) and return the concrete node.
/// Nodes without a `$ref` are returned unchanged.
pub fn deref<'a>(doc: &'a Value, mut node: &'a Value) -> &'a Value {
    let mut depth = 0;
    while let Some(r) = node.get("$ref").and_then(Value::as_str) {
        if depth >= MAX_DEREF_DEPTH {
            break;
        }
        match resolve_pointer(doc, r) {
            Some(target) => node = target,
            None => break,
        }
        depth += 1;
    }
    node
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn derefs_local_ref() {
        let doc = json!({
            "components": { "schemas": { "Pet": { "type": "object" } } },
            "node": { "$ref": "#/components/schemas/Pet" }
        });
        let node = deref(&doc, &doc["node"]);
        assert_eq!(node["type"], "object");
    }

    #[test]
    fn survives_ref_cycles() {
        let doc = json!({
            "components": { "schemas": {
                "A": { "$ref": "#/components/schemas/B" },
                "B": { "$ref": "#/components/schemas/A" }
            } },
            "node": { "$ref": "#/components/schemas/A" }
        });
        // must terminate
        let _ = deref(&doc, &doc["node"]);
    }

    #[test]
    fn passes_through_concrete_nodes() {
        let doc = json!({"node": {"type": "string"}});
        let node = deref(&doc, &doc["node"]);
        assert_eq!(node["type"], "string");
    }
}
