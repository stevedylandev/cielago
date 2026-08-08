//! OAuth 2.0 client-credentials flow (RFC 6749 §4.4).

use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use super::secret::resolve_secret;
use crate::model::{AuthStyle, OAuthConfig};

/// Clock skew so tokens are refreshed slightly before their stated expiry.
const EXPIRY_SKEW_SECS: u64 = 30;

#[derive(Debug, Clone)]
pub struct OAuthToken {
    pub access_token: String,
    pub expires_at: Instant,
}

pub fn token_valid(token: &OAuthToken) -> bool {
    Instant::now() < token.expires_at
}

/// Request a new access token using the client-credentials grant.
pub async fn fetch_token(client: &reqwest::Client, cfg: &OAuthConfig) -> Result<OAuthToken> {
    if cfg.token_url.is_empty() {
        bail!("OAuth token URL is not configured (press A to configure auth)");
    }

    let mut form: Vec<(&str, String)> = vec![("grant_type", "client_credentials".into())];
    if !cfg.scopes.is_empty() {
        form.push(("scope", cfg.scopes.join(" ")));
    }

    // The client secret may be a `$(…)` command (e.g. a password manager read);
    // resolve it just before the exchange so it never sits in memory longer.
    let client_secret =
        resolve_secret(&cfg.client_secret).context("resolving OAuth client secret")?;

    let mut rb = client.post(&cfg.token_url);
    match cfg.auth_style {
        AuthStyle::Basic => {
            rb = rb.basic_auth(cfg.client_id.clone(), Some(client_secret));
        }
        AuthStyle::Post => {
            form.push(("client_id", cfg.client_id.clone()));
            form.push(("client_secret", client_secret));
        }
    }

    let resp = rb
        .form(&form)
        .send()
        .await
        .context("token request failed")?;
    let status = resp.status();
    let text = resp.text().await.context("reading token response")?;
    if !status.is_success() {
        bail!("token request returned {status}: {text}");
    }

    let v: Value = serde_json::from_str(&text).context("token response is not JSON")?;
    let access_token = v
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("token response missing access_token"))?
        .to_string();
    let expires_in = v.get("expires_in").and_then(Value::as_u64).unwrap_or(3600);

    Ok(OAuthToken {
        access_token,
        expires_at: Instant::now()
            + Duration::from_secs(expires_in.saturating_sub(EXPIRY_SKEW_SECS)),
    })
}
