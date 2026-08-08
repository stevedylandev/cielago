//! Resolving secret values that shell out, e.g.
//! `$(op read "op://vault/item/field")`.

use anyhow::{Context, Result, bail};
use std::process::Command;

/// If `value` is *entirely* a single `$(…)` command substitution, run the inner
/// command through `sh -c` and return its trimmed stdout. Anything else is
/// returned unchanged — the whole value must be the substitution, so a literal
/// `$(...)` embedded in a longer string is never executed by accident.
pub fn resolve_secret(value: &str) -> Result<String> {
    let Some(cmd) = command_substitution(value) else {
        return Ok(value.to_string());
    };

    let output = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .with_context(|| format!("running secret command `{cmd}`"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "secret command `{cmd}` failed ({}): {}",
            output.status,
            stderr.trim()
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The command inside a value shaped exactly like `$( … )`, or `None`.
fn command_substitution(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    let inner = trimmed.strip_prefix("$(")?.strip_suffix(')')?.trim();
    (!inner.is_empty()).then_some(inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_values_pass_through() {
        assert_eq!(resolve_secret("hunter2").unwrap(), "hunter2");
        // A `$(…)` that isn't the whole value is left untouched.
        assert_eq!(
            resolve_secret("Bearer $(echo x)").unwrap(),
            "Bearer $(echo x)"
        );
        assert_eq!(resolve_secret("$()").unwrap(), "$()");
    }

    #[test]
    fn command_substitution_runs_and_trims() {
        assert_eq!(resolve_secret("$(printf 'sk-123')").unwrap(), "sk-123");
        // Surrounding whitespace on the value and on the output are both dropped.
        assert_eq!(resolve_secret("  $(echo padded)  ").unwrap(), "padded");
    }

    #[test]
    fn failing_command_is_an_error() {
        let err = resolve_secret("$(exit 3)").unwrap_err().to_string();
        assert!(err.contains("failed"), "unexpected error: {err}");
    }
}
