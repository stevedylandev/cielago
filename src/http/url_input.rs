//! Decompose a URL typed or pasted into the URL bar. This is the inverse of
//! [`super::client`]'s `build_url`: that composes `base + path + query` into a
//! request, this splits a pasted URL back into the pieces a
//! [`crate::model::SavedRequest`] stores.

use url::Url;

use crate::model::KeyValueRow;

/// The pieces a URL-bar entry decomposes into.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UrlParts {
    /// Origin (`scheme://host[:port]`) when an absolute http(s) URL was pasted;
    /// `None` for a bare path, which is what the field normally holds.
    pub origin: Option<String>,
    /// Always leading-slash (or a `{{var}}` template), relative to `origin`.
    pub path: String,
    /// `None` when the input had no `?` at all, meaning "leave the existing
    /// query rows alone"; `Some(rows)` when it did, even if empty — a bare `?`
    /// clears them.
    pub query: Option<Vec<KeyValueRow>>,
}

pub fn split_url_input(input: &str) -> UrlParts {
    let input = input.trim();
    if input.is_empty() {
        return UrlParts {
            origin: None,
            path: "/".into(),
            query: None,
        };
    }

    // `Url::parse` succeeds for anything with a scheme, including nonsense like
    // `localhost:8080/pets` (scheme `localhost`, path `8080/pets`), so the
    // scheme and host checks are load-bearing rather than cosmetic.
    if let Ok(u) = Url::parse(input)
        && matches!(u.scheme(), "http" | "https")
        && u.host().is_some()
    {
        return UrlParts {
            origin: Some(u.origin().ascii_serialization()),
            path: restore_braces(u.path()),
            query: u.query().map(parse_query),
            // The fragment is deliberately dropped: it is never sent to a server.
        };
    }

    let no_fragment = input.split_once('#').map_or(input, |(head, _)| head);
    let (path, query) = match no_fragment.split_once('?') {
        Some((p, q)) => (p, Some(parse_query(q))),
        None => (no_fragment, None),
    };
    // `{{baseUrl}}/pets` is a template, not a relative path — leave it verbatim.
    let path = if path.is_empty() {
        "/".to_string()
    } else if path.starts_with('/') || path.starts_with("{{") {
        path.to_string()
    } else {
        format!("/{path}")
    };
    UrlParts {
        origin: None,
        path,
        query,
    }
}

/// Undo the `url` crate's percent-encoding of `{` and `}`, which are in its
/// path encode set — so `/pets/{id}` comes back as `/pets/%7Bid%7D` and would
/// break both `{pathParam}` replacement and `{{variable}}` substitution. Only
/// the braces are restored; a blanket decode would corrupt segments the user
/// percent-encoded on purpose.
fn restore_braces(s: &str) -> String {
    s.replace("%7B", "{")
        .replace("%7b", "{")
        .replace("%7D", "}")
        .replace("%7d", "}")
}

/// Query string to enabled rows. Values are decoded here and re-encoded by
/// `build_url`'s `.query(&pairs)`, so they round-trip.
fn parse_query(raw: &str) -> Vec<KeyValueRow> {
    url::form_urlencoded::parse(raw.as_bytes())
        .map(|(k, v)| KeyValueRow::new(k.as_ref(), v.as_ref(), true))
        .filter(|r| !r.key.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(parts: &UrlParts) -> Vec<(String, String)> {
        parts
            .query
            .as_ref()
            .map(|q| q.iter().map(|r| (r.key.clone(), r.value.clone())).collect())
            .unwrap_or_default()
    }

    #[test]
    fn splits_a_full_url() {
        let p = split_url_input("https://api.example.com/v1/pets?limit=10");
        assert_eq!(p.origin.as_deref(), Some("https://api.example.com"));
        assert_eq!(p.path, "/v1/pets");
        assert_eq!(rows(&p), vec![("limit".to_string(), "10".to_string())]);
    }

    #[test]
    fn bare_path_has_no_origin() {
        let p = split_url_input("/v1/pets");
        assert_eq!(p.origin, None);
        assert_eq!(p.path, "/v1/pets");
        assert_eq!(p.query, None);
    }

    #[test]
    fn relative_path_gains_a_leading_slash() {
        assert_eq!(split_url_input("pets/42").path, "/pets/42");
    }

    #[test]
    fn host_with_port_and_no_scheme_is_relative() {
        // `Url::parse` accepts this with scheme "localhost"; we must not.
        let p = split_url_input("localhost:8080/pets");
        assert_eq!(p.origin, None);
        assert_eq!(p.path, "/localhost:8080/pets");
    }

    #[test]
    fn keeps_brace_placeholders_unencoded() {
        let p = split_url_input("https://api.example.com/pets/{petId}/photos");
        assert_eq!(p.path, "/pets/{petId}/photos");
    }

    #[test]
    fn keeps_double_brace_variables_in_a_relative_path() {
        let p = split_url_input("{{prefix}}/pets");
        assert_eq!(p.origin, None);
        assert_eq!(p.path, "{{prefix}}/pets");
    }

    #[test]
    fn drops_the_fragment() {
        assert_eq!(
            split_url_input("https://api.example.com/pets#section").path,
            "/pets"
        );
        assert_eq!(split_url_input("/pets#section").path, "/pets");
        assert_eq!(split_url_input("/pets?a=1#section").path, "/pets");
        assert_eq!(
            rows(&split_url_input("/pets?a=1#section")),
            vec![("a".to_string(), "1".to_string())]
        );
    }

    #[test]
    fn empty_input_becomes_root_path() {
        let p = split_url_input("   ");
        assert_eq!(p.path, "/");
        assert_eq!(p.origin, None);
        assert_eq!(p.query, None);
    }

    #[test]
    fn absent_question_mark_leaves_query_none() {
        assert_eq!(split_url_input("https://api.example.com/pets").query, None);
        assert_eq!(split_url_input("/pets").query, None);
    }

    #[test]
    fn bare_question_mark_clears_the_query() {
        assert_eq!(split_url_input("/pets?").query, Some(Vec::new()));
        assert_eq!(
            split_url_input("https://api.example.com/pets?").query,
            Some(Vec::new())
        );
    }

    #[test]
    fn decodes_query_values() {
        let p = split_url_input("/search?q=hello%20world&tag=a%2Bb");
        assert_eq!(
            rows(&p),
            vec![
                ("q".to_string(), "hello world".to_string()),
                ("tag".to_string(), "a+b".to_string()),
            ]
        );
    }

    #[test]
    fn default_ports_are_dropped_from_the_origin() {
        assert_eq!(
            split_url_input("https://api.example.com:443/pets")
                .origin
                .as_deref(),
            Some("https://api.example.com")
        );
        assert_eq!(
            split_url_input("http://localhost:8080/pets")
                .origin
                .as_deref(),
            Some("http://localhost:8080")
        );
    }

    #[test]
    fn query_rows_are_enabled_and_skip_empty_keys() {
        let p = split_url_input("/pets?a=1&=2&b");
        let q = p.query.unwrap();
        assert_eq!(q.len(), 2);
        assert!(q.iter().all(|r| r.enabled));
        assert_eq!(q[1].key, "b");
        assert_eq!(q[1].value, "");
    }
}
