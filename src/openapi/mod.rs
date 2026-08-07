//! OpenAPI 3.x (3.0 + 3.1, JSON/YAML) loading and conversion into collections.
//!
//! Specs are parsed into [`serde_json::Value`] rather than a strict spec model
//! so that unknown fields and 3.0/3.1 differences are tolerated gracefully.

pub mod docs;
pub mod examples;
pub mod import;
pub mod loader;
pub mod resolve;

pub use import::import_spec;
pub use loader::{load_spec, parse_spec};
