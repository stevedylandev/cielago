use anyhow::{Context, Result};
use serde_json::Value;

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

/// Parse spec text as JSON, falling back to YAML.
pub fn parse_spec(text: &str) -> Result<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        return Ok(v);
    }
    let v: Value = serde_yaml::from_str(text).context("spec is neither valid JSON nor YAML")?;
    Ok(v)
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
    fn rejects_garbage() {
        assert!(parse_spec("\u{1}\u{2}not a spec at all: [").is_err());
    }
}
