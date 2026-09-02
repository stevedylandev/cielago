use anyhow::{Context, Result};
use serde_json::Value;

use super::swagger2;

/// Load a spec from a local file path or an http(s) URL.
pub async fn load_spec(source: &str) -> Result<Value> {
    if source.starts_with("http://") || source.starts_with("https://") {
        let client = reqwest::Client::new();
        let text = client
            .get(source)
            .send()
            .await
            .with_context(|| format!("fetching {source}"))?
            .error_for_status()
            .with_context(|| format!("fetching {source}"))?
            .text()
            .await
            .with_context(|| format!("reading body of {source}"))?;
        parse_spec(&text).with_context(|| format!("parsing spec from {source}"))
    } else {
        let text = std::fs::read_to_string(source).with_context(|| format!("reading {source}"))?;
        parse_spec(&text).with_context(|| format!("parsing spec from {source}"))
    }
}

/// Parse spec text as JSON, falling back to YAML. A Swagger 2.0 document is
/// converted to its OpenAPI 3.0 equivalent here, so every caller downstream
/// only has to understand one dialect.
pub fn parse_spec(text: &str) -> Result<Value> {
    let v = match serde_json::from_str::<Value>(text) {
        Ok(v) => v,
        Err(_) => serde_yaml::from_str(text).context("spec is neither valid JSON nor YAML")?,
    };
    Ok(if swagger2::is_swagger2(&v) {
        swagger2::to_openapi3(v)
    } else {
        v
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_and_yaml() {
        let json = r#"{"openapi":"3.0.0"}"#;
        assert_eq!(parse_spec(json).unwrap()["openapi"], "3.0.0");

        let yaml = "openapi: 3.1.0\ninfo:\n  title: t\n";
        let v = parse_spec(yaml).unwrap();
        assert_eq!(v["openapi"], "3.1.0");
        assert_eq!(v["info"]["title"], "t");
    }

    #[test]
    fn converts_swagger_2_on_parse() {
        let yaml = "swagger: '2.0'\nhost: api.example.com\nbasePath: /v1\nschemes: [https]\n";
        let v = parse_spec(yaml).unwrap();
        assert_eq!(v["openapi"], "3.0.0");
        assert!(v.get("swagger").is_none());
        assert_eq!(v["servers"][0]["url"], "https://api.example.com/v1");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_spec("\u{1}\u{2}not a spec at all: [").is_err());
    }
}
