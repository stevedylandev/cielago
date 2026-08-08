use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// The saved view records which pane and editor tab were open, so the two
// enums that describe them are re-used here rather than mirrored.
use crate::app::{EditorTab, Focus};

/// Collection-level `{{variables}}` as an ordered, editable list.
pub type Variables = Vec<KeyValueRow>;

pub fn variables_map(vars: &Variables) -> HashMap<String, String> {
    vars.iter()
        .filter(|r| r.enabled && !r.key.is_empty())
        .map(|r| (r.key.clone(), r.value.clone()))
        .collect()
}

/// Names inside single `{…}` in a path template, deduped, in order of first
/// appearance. `{{var}}` is variable syntax rather than a path param and is
/// skipped, so `/{{tenant}}/pets/{petId}` yields just `petId`.
pub fn path_placeholders(path: &str) -> Vec<String> {
    let bytes = path.as_bytes();
    let mut names: Vec<String> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'{' {
            i += 1;
            continue;
        }
        // `{{…}}` is a variable; skip past its closing braces entirely.
        if bytes.get(i + 1) == Some(&b'{') {
            match path[i + 2..].find("}}") {
                Some(off) => i += 2 + off + 2,
                None => break,
            }
            continue;
        }
        let Some(off) = path[i + 1..].find('}') else {
            break;
        };
        let name = path[i + 1..i + 1 + off].trim();
        if !name.is_empty() && !name.contains('/') && !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
        i += 1 + off + 1;
    }
    names
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collection {
    pub name: String,
    /// Path or URL the spec was imported from, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_source: Option<String>,
    #[serde(default)]
    pub servers: Vec<String>,
    #[serde(default)]
    pub active_server: usize,
    #[serde(default)]
    pub variables: Variables,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<OAuthConfig>,
    /// How the sidebar labels requests; persisted with the collection.
    #[serde(default)]
    pub label_mode: LabelMode,
    /// Whether tag groups start collapsed when the collection is opened.
    #[serde(default)]
    pub groups_collapsed: bool,
    /// The request that was open the last time the collection was saved, so
    /// reopening it lands back on the same page. `None` for collections saved
    /// before this existed, or saved with nothing selected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_request: Option<Uuid>,
    /// Pane (`1`/`2`/`3`) that had focus at the last save.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_focus: Option<Focus>,
    /// Editor tab that was open at the last save.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_tab: Option<EditorTab>,
    #[serde(default)]
    pub requests: Vec<SavedRequest>,
}

impl Collection {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            spec_source: None,
            servers: Vec::new(),
            active_server: 0,
            variables: Vec::new(),
            auth: None,
            label_mode: LabelMode::default(),
            groups_collapsed: false,
            last_request: None,
            last_focus: None,
            last_tab: None,
            requests: Vec::new(),
        }
    }

    pub fn base_url(&self) -> Option<&str> {
        self.servers.get(self.active_server).map(|s| s.as_str())
    }

    /// Take the routes from a freshly imported collection, overwriting this
    /// collection's `requests` while leaving auth, variables, servers, active
    /// server and view state as they were. `last_request` is dropped because
    /// import mints new request ids, so an old pointer would dangle.
    pub fn replace_requests_from(&mut self, imported: Collection) {
        self.requests = imported.requests;
        self.last_request = None;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AuthStyle {
    /// client_id/client_secret sent via HTTP Basic header (RFC 6749 default).
    #[default]
    Basic,
    /// client_id/client_secret sent in the form body.
    Post,
}

/// Which authentication scheme a collection uses. `Oauth2` is the default so
/// collections written before bearer/api-key support (their `auth` object has
/// no `kind`) keep loading as the client-credentials config they were.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AuthKind {
    #[default]
    Oauth2,
    /// A fixed bearer token sent as `Authorization: Bearer <token>`.
    Bearer,
    /// A fixed value sent in an arbitrary header (defaults to `X-API-Key`).
    #[serde(rename = "apikey")]
    ApiKey,
}

impl AuthKind {
    pub const ALL: [AuthKind; 3] = [AuthKind::Bearer, AuthKind::ApiKey, AuthKind::Oauth2];

    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|k| *k == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    pub fn title(self) -> &'static str {
        match self {
            AuthKind::Oauth2 => "oauth2",
            AuthKind::Bearer => "bearer",
            AuthKind::ApiKey => "apikey",
        }
    }
}

/// Header used for API-key auth when the user leaves the header name blank.
pub const DEFAULT_API_KEY_HEADER: &str = "X-API-Key";

/// Per-collection authentication. One struct carries every scheme's fields
/// (discriminated by `kind`) so the on-disk shape stays a single flat object
/// and older OAuth-only collections deserialize unchanged.
///
/// Secret-bearing fields (`token`, `client_secret`) may hold a `$(…)` command
/// substitution, resolved at send time — see [`crate::http::resolve_secret`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthConfig {
    #[serde(default)]
    pub kind: AuthKind,
    /// Bearer token, or API-key value (secret; supports `$(…)`).
    #[serde(default)]
    pub token: String,
    /// Header name for `ApiKey` auth; empty means [`DEFAULT_API_KEY_HEADER`].
    #[serde(default)]
    pub header: String,
    #[serde(default)]
    pub token_url: String,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub auth_style: AuthStyle,
}

/// Historical name; `OAuthConfig` now covers every scheme via its `kind`.
pub type AuthConfig = OAuthConfig;

impl OAuthConfig {
    /// Whether the active scheme has enough filled in to attempt auth.
    pub fn is_configured(&self) -> bool {
        match self.kind {
            AuthKind::Oauth2 => !self.token_url.is_empty() && !self.client_id.is_empty(),
            AuthKind::Bearer | AuthKind::ApiKey => !self.token.is_empty(),
        }
    }

    /// The header name to carry an API key, falling back to the default.
    pub fn api_key_header(&self) -> &str {
        if self.header.trim().is_empty() {
            DEFAULT_API_KEY_HEADER
        } else {
            self.header.trim()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
}

impl Method {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "get" => Self::Get,
            "post" => Self::Post,
            "put" => Self::Put,
            "patch" => Self::Patch,
            "delete" => Self::Delete,
            "head" => Self::Head,
            "options" => Self::Options,
            _ => return None,
        })
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
            Self::Head => "HEAD",
            Self::Options => "OPTIONS",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyValueRow {
    pub key: String,
    #[serde(default)]
    pub value: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

impl KeyValueRow {
    pub fn new(key: impl Into<String>, value: impl Into<String>, enabled: bool) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            enabled,
        }
    }
}

/// Spec-derived documentation for one input to a request: a parameter, or a
/// field of the request body. Stored on the request (rather than looked up in
/// the spec on demand) so the Docs tab works for collections whose spec is a
/// URL that may be gone, moved or behind auth by the time you open them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDoc {
    /// Parameter name, or dotted path for a body field (`owner.address[].zip`).
    pub name: String,
    /// `path`, `query`, `header` or `body`.
    pub location: String,
    /// Rendered type, e.g. `string(uuid)`, `integer`, `array<string>`.
    #[serde(default)]
    pub ty: String,
    #[serde(default)]
    pub required: bool,
    /// `enum` values — the "options" this field accepts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

/// How the sidebar labels a request. Spec-derived names (`operationId`) are
/// often long and unreadable, so the label is a view concern, independent of
/// the stored `name`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LabelMode {
    /// The request's `name` (renameable with `r`).
    #[default]
    Name,
    /// The spec's `summary`, falling back to `name`.
    Summary,
    /// The path template, e.g. `/pets/{petId}`.
    Path,
}

impl LabelMode {
    pub const ALL: [LabelMode; 3] = [LabelMode::Name, LabelMode::Summary, LabelMode::Path];

    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|m| *m == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    pub fn title(self) -> &'static str {
        match self {
            LabelMode::Name => "name",
            LabelMode::Summary => "summary",
            LabelMode::Path => "path",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedRequest {
    pub id: Uuid,
    pub name: String,
    /// The spec's `summary` for this operation, kept so the sidebar can label
    /// requests by it without destroying a user-chosen `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// The spec's `operationId`, kept for the same reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    /// The spec's operation `description`, shown in the Docs tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Types, enums and descriptions for params and body fields (Docs tab).
    /// Empty for hand-made requests and for collections imported before this
    /// existed — re-importing the spec fills it in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub docs: Vec<FieldDoc>,
    pub method: Method,
    /// Path template, may contain `{param}` placeholders and `{{variables}}`.
    pub path: String,
    #[serde(default)]
    pub path_params: Vec<KeyValueRow>,
    #[serde(default)]
    pub query: Vec<KeyValueRow>,
    #[serde(default)]
    pub headers: Vec<KeyValueRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl SavedRequest {
    pub fn blank(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            summary: None,
            operation_id: None,
            description: None,
            docs: Vec::new(),
            method: Method::Get,
            path: "/".into(),
            path_params: Vec::new(),
            query: Vec::new(),
            headers: Vec::new(),
            body: None,
            tags: Vec::new(),
        }
    }

    /// Rewrite `path_params` to hold exactly one row per `{placeholder}` in
    /// `path`, in path order. Surviving rows keep their value and `enabled`
    /// flag; rows whose placeholder is gone are dropped, since
    /// [`crate::http::client`]'s `build_url` ignores them anyway and a stale
    /// row only makes the Params table lie. Returns whether anything changed.
    pub fn sync_path_params(&mut self) -> bool {
        let names = path_placeholders(&self.path);
        let synced: Vec<KeyValueRow> = names
            .iter()
            .map(|name| {
                self.path_params
                    .iter()
                    .find(|r| &r.key == name)
                    .cloned()
                    .unwrap_or_else(|| KeyValueRow::new(name.clone(), "", true))
            })
            .collect();
        let changed = synced != self.path_params;
        if changed {
            self.path_params = synced;
        }
        changed
    }

    /// Sidebar label under the collection's current [`LabelMode`]. Every mode
    /// falls back to something non-empty so rows are never blank.
    pub fn label(&self, mode: LabelMode) -> &str {
        let candidate = match mode {
            LabelMode::Name => Some(self.name.as_str()),
            LabelMode::Summary => self.summary.as_deref(),
            LabelMode::Path => Some(self.path.as_str()),
        };
        candidate
            .filter(|s| !s.is_empty())
            .unwrap_or(if self.name.is_empty() {
                self.path.as_str()
            } else {
                self.name.as_str()
            })
    }

    /// Lowercased haystack for sidebar search: label fields plus method/tags.
    pub fn matches(&self, needle: &str) -> bool {
        let needle = needle.to_ascii_lowercase();
        let fields = [
            self.name.as_str(),
            self.path.as_str(),
            self.summary.as_deref().unwrap_or(""),
            self.operation_id.as_deref().unwrap_or(""),
        ];
        fields
            .iter()
            .any(|f| f.to_ascii_lowercase().contains(&needle))
            || self
                .method
                .to_string()
                .to_ascii_lowercase()
                .contains(&needle)
            || self
                .tags
                .iter()
                .any(|t| t.to_ascii_lowercase().contains(&needle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(rows: &[KeyValueRow]) -> Vec<&str> {
        rows.iter().map(|r| r.key.as_str()).collect()
    }

    #[test]
    fn path_placeholders_finds_single_braces() {
        assert_eq!(
            path_placeholders("/pets/{petId}/photos/{photoId}"),
            vec!["petId", "photoId"]
        );
        assert!(path_placeholders("/pets").is_empty());
        // Deduped, and blank or path-spanning braces are ignored.
        assert_eq!(path_placeholders("/a/{id}/b/{id}"), vec!["id"]);
        assert!(path_placeholders("/a/{}/b").is_empty());
        assert!(path_placeholders("/a/{oops/b}").is_empty());
        // An unterminated brace ends the scan rather than looping.
        assert!(path_placeholders("/pets/{petId").is_empty());
    }

    #[test]
    fn path_placeholders_skips_double_brace_variables() {
        assert_eq!(path_placeholders("/{{tenant}}/pets/{petId}"), vec!["petId"]);
        assert!(path_placeholders("{{baseUrl}}/pets").is_empty());
        assert!(path_placeholders("/a/{{unterminated").is_empty());
    }

    #[test]
    fn sync_path_params_adds_prunes_and_reorders() {
        let mut req = SavedRequest::blank("r");
        req.path = "/orgs/{orgId}/pets/{petId}".into();
        req.path_params = vec![
            KeyValueRow::new("petId", "42", true),
            KeyValueRow::new("stale", "x", true),
        ];

        assert!(req.sync_path_params());
        assert_eq!(keys(&req.path_params), vec!["orgId", "petId"]);
        // A second call is a no-op.
        assert!(!req.sync_path_params());

        req.path = "/pets".into();
        assert!(req.sync_path_params());
        assert!(req.path_params.is_empty());
    }

    #[test]
    fn sync_path_params_keeps_existing_values_and_flags() {
        let mut req = SavedRequest::blank("r");
        req.path = "/pets/{petId}".into();
        req.path_params = vec![KeyValueRow::new("petId", "42", false)];

        assert!(!req.sync_path_params());
        assert_eq!(req.path_params[0].value, "42");
        assert!(!req.path_params[0].enabled);
    }
}
