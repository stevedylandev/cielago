use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::model::Collection;

/// Root config directory: `~/.config/cielago` on every platform, matching the
/// documented storage layout (rather than e.g. `~/Library/Application Support`
/// on macOS).
pub fn config_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
    let dir = home.join(".config").join("cielago");
    migrate_legacy_dirs(&home.join(".config"), &dir);
    Ok(dir)
}

/// Names this project shipped under before `cielago`, newest first.
const LEGACY_DIR_NAMES: [&str; 3] = ["manpost", "stableman", "getman"];

/// Move a leftover directory from an earlier name onto the current one, so
/// existing collections survive the rename. No-op once `cielago` exists.
fn migrate_legacy_dirs(config_root: &std::path::Path, new: &PathBuf) {
    if new.exists() {
        return;
    }
    for legacy in LEGACY_DIR_NAMES {
        let old = config_root.join(legacy);
        if old.exists() && fs::rename(&old, new).is_ok() {
            return;
        }
    }
}

pub fn collections_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join("collections"))
}

/// Filesystem-safe slug for a collection name.
pub fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !slug.is_empty() {
            slug.push('-');
            last_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "collection".into()
    } else {
        slug
    }
}

pub fn collection_path(name: &str) -> Result<PathBuf> {
    Ok(collections_dir()?.join(format!("{}.json", slugify(name))))
}

pub fn save_collection(collection: &Collection) -> Result<PathBuf> {
    let dir = collections_dir()?;
    fs::create_dir_all(&dir).context("creating collections directory")?;
    let path = collection_path(&collection.name)?;
    let json = serde_json::to_string_pretty(collection)?;
    fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

pub fn load_collection(name: &str) -> Result<Collection> {
    let path = collection_path(name)?;
    load_collection_path(&path)
}

pub fn load_collection_path(path: &PathBuf) -> Result<Collection> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let collection = serde_json::from_str(&text)
        .with_context(|| format!("parsing collection at {}", path.display()))?;
    Ok(collection)
}

/// Resolve a user-typed collection name onto a saved one. Exact matches win;
/// otherwise anything that slugifies the same does, so `cielago delete "some
/// api"` finds `Some API`.
pub fn resolve_collection(name: &str) -> Result<String> {
    let names = list_collections()?;
    if let Some(found) = match_name(&names, name) {
        return Ok(found);
    }
    if names.is_empty() {
        bail!(
            "No collections yet. Import one:\n\n  cielago import <spec.json|yaml|url>\n\nOr create an empty one:\n\n  cielago new <name>"
        )
    }
    bail!(
        "No collection named {name:?}.\n\nAvailable: {}",
        names.join(", ")
    )
}

/// Pick the saved name a user-typed one refers to: exact match first, then any
/// name with the same slug (which is what the file is named after anyway).
fn match_name(names: &[String], input: &str) -> Option<String> {
    if names.iter().any(|n| n == input) {
        return Some(input.to_string());
    }
    let slug = slugify(input);
    names.iter().find(|n| slugify(n) == slug).cloned()
}

/// Delete a saved collection. Returns the file that was removed.
pub fn delete_collection(name: &str) -> Result<PathBuf> {
    let path = collection_path(name)?;
    fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    Ok(path)
}

/// Names of all saved collections (derived from file names).
pub fn list_collections() -> Result<Vec<String>> {
    let dir = collections_dir()?;
    let mut names = Vec::new();
    if dir.exists() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json")
                && let Ok(c) = load_collection_path(&path)
            {
                names.push(c.name);
            }
        }
    }
    names.sort();
    Ok(names)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub last_collection: Option<String>,
    #[serde(default)]
    pub editor: Option<String>,
}

impl AppConfig {
    fn path() -> Result<PathBuf> {
        Ok(config_dir()?.join("config.json"))
    }

    pub fn load() -> AppConfig {
        Self::path()
            .and_then(|p| Ok(fs::read_to_string(p)?))
            .and_then(|t| Ok(serde_json::from_str(&t)?))
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let dir = config_dir()?;
        fs::create_dir_all(&dir)?;
        fs::write(Self::path()?, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Editor to use for external body editing: config override, `$EDITOR`, else `vi`.
    pub fn editor_cmd(&self) -> String {
        self.editor
            .clone()
            .or_else(|| std::env::var("EDITOR").ok())
            .filter(|e| !e.is_empty())
            .unwrap_or_else(|| "vi".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Method;

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("My Pet API"), "my-pet-api");
        assert_eq!(slugify("pets_v2 (internal)"), "pets-v2-internal");
        assert_eq!(slugify("!!!"), "collection");
        assert_eq!(slugify("a"), "a");
    }

    #[test]
    fn match_name_exact_then_slug() {
        let names = vec!["Some API".to_string(), "Other API".to_string()];
        assert_eq!(match_name(&names, "Some API").as_deref(), Some("Some API"));
        assert_eq!(match_name(&names, "some api").as_deref(), Some("Some API"));
        assert_eq!(match_name(&names, "some-api").as_deref(), Some("Some API"));
        assert_eq!(match_name(&names, "nope"), None);
    }

    #[test]
    fn collection_json_roundtrip() {
        let mut c = Collection::new("Test API");
        c.servers = vec![
            "https://a.example.com".into(),
            "https://b.example.com".into(),
        ];
        c.active_server = 1;
        c.variables
            .push(crate::model::KeyValueRow::new("tenant", "acme", true));
        c.auth = Some(crate::model::OAuthConfig {
            token_url: "https://auth.example.com/token".into(),
            client_id: "id".into(),
            client_secret: "secret".into(),
            scopes: vec!["read".into()],
            auth_style: crate::model::AuthStyle::Basic,
        });
        let mut r = crate::model::SavedRequest::blank("list pets");
        r.method = Method::Post;
        r.query
            .push(crate::model::KeyValueRow::new("limit", "10", false));
        r.body = Some("{\"a\":1}".into());
        c.requests.push(r);

        let json = serde_json::to_string_pretty(&c).unwrap();
        let back: Collection = serde_json::from_str(&json).unwrap();

        assert_eq!(back.name, "Test API");
        assert_eq!(back.base_url(), Some("https://b.example.com"));
        assert_eq!(back.variables[0].value, "acme");
        assert_eq!(back.auth.as_ref().unwrap().client_secret, "secret");
        assert_eq!(back.requests.len(), 1);
        assert_eq!(back.requests[0].method, Method::Post);
        assert!(!back.requests[0].query[0].enabled);
    }
}
