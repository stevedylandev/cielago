//! App-level send orchestration: pick the auth scheme, and for OAuth cache the
//! token + retry once on 401.

use std::collections::HashMap;

use super::client::{HttpResponse, send_request};
use super::oauth::{OAuthToken, fetch_token, token_valid};
use super::secret::resolve_secret;
use crate::model::{AuthKind, OAuthConfig, SavedRequest};

pub struct SendOutcome {
    pub result: Result<HttpResponse, String>,
    /// Latest OAuth token cache (unchanged on failure, refreshed on (re)fetch).
    /// Always `None` for the bearer/API-key schemes, which hold no cache.
    pub token: Option<OAuthToken>,
}

/// Send a request under the collection's auth scheme:
/// - **none** — send as-is.
/// - **bearer** — resolve the token (may be a `$(…)` secret) and send it as
///   `Authorization: Bearer …`.
/// - **apikey** — resolve the value and send it in the configured header.
/// - **oauth2** — reuse a cached client-credentials token while valid, fetch
///   one otherwise, and retry the request once with a fresh token on a 401.
pub async fn send_with_auth(
    client: &reqwest::Client,
    base_url: &str,
    req: &SavedRequest,
    vars: &HashMap<String, String>,
    auth: Option<&OAuthConfig>,
    cached: Option<OAuthToken>,
) -> SendOutcome {
    let Some(cfg) = auth.filter(|c| c.is_configured()) else {
        let result = send_request(client, base_url, req, vars, None, &[])
            .await
            .map_err(|e| format!("{e:#}"));
        return SendOutcome {
            result,
            token: cached,
        };
    };

    match cfg.kind {
        AuthKind::Bearer => {
            let token = match resolve_secret(&cfg.token) {
                Ok(t) => t,
                Err(e) => return secret_error(e, cached),
            };
            let result = send_request(client, base_url, req, vars, Some(&token), &[])
                .await
                .map_err(|e| format!("{e:#}"));
            SendOutcome {
                result,
                token: cached,
            }
        }
        AuthKind::ApiKey => {
            let value = match resolve_secret(&cfg.token) {
                Ok(v) => v,
                Err(e) => return secret_error(e, cached),
            };
            let extra = [(cfg.api_key_header().to_string(), value)];
            let result = send_request(client, base_url, req, vars, None, &extra)
                .await
                .map_err(|e| format!("{e:#}"));
            SendOutcome {
                result,
                token: cached,
            }
        }
        AuthKind::Oauth2 => oauth_send(client, base_url, req, vars, cfg, cached).await,
    }
}

fn secret_error(e: anyhow::Error, token: Option<OAuthToken>) -> SendOutcome {
    SendOutcome {
        result: Err(format!("secret resolution failed: {e:#}")),
        token,
    }
}

async fn oauth_send(
    client: &reqwest::Client,
    base_url: &str,
    req: &SavedRequest,
    vars: &HashMap<String, String>,
    cfg: &OAuthConfig,
    cached: Option<OAuthToken>,
) -> SendOutcome {
    let mut token = cached;

    let stale = token.as_ref().map(|t| !token_valid(t)).unwrap_or(true);
    if stale {
        match fetch_token(client, cfg).await {
            Ok(t) => token = Some(t),
            Err(e) => {
                return SendOutcome {
                    result: Err(format!("token fetch failed: {e:#}")),
                    token,
                };
            }
        }
    }
    let mut bearer = token.as_ref().map(|t| t.access_token.clone());

    let mut resp = send_request(client, base_url, req, vars, bearer.as_deref(), &[])
        .await
        .map_err(|e| format!("{e:#}"));

    if let Ok(r) = &resp
        && r.status == 401
        && let Ok(t) = fetch_token(client, cfg).await
    {
        bearer = Some(t.access_token.clone());
        token = Some(t);
        resp = send_request(client, base_url, req, vars, bearer.as_deref(), &[])
            .await
            .map_err(|e| format!("{e:#}"));
    }

    SendOutcome {
        result: resp,
        token,
    }
}
