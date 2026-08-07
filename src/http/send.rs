//! App-level send orchestration: OAuth token caching + one 401 retry.

use std::collections::HashMap;

use super::client::{HttpResponse, send_request};
use super::oauth::{OAuthToken, fetch_token, token_valid};
use crate::model::{OAuthConfig, SavedRequest};

pub struct SendOutcome {
    pub result: Result<HttpResponse, String>,
    /// Latest token cache (unchanged on failure, refreshed on (re)fetch).
    pub token: Option<OAuthToken>,
}

/// Send a request, transparently handling OAuth client-credentials auth:
/// reuse a cached token while valid, fetch one otherwise, and retry the
/// request once with a fresh token on a 401 response.
pub async fn send_with_auth(
    client: &reqwest::Client,
    base_url: &str,
    req: &SavedRequest,
    vars: &HashMap<String, String>,
    auth: Option<&OAuthConfig>,
    cached: Option<OAuthToken>,
) -> SendOutcome {
    let mut token = cached;
    let auth = auth.filter(|c| c.is_configured());

    let mut bearer: Option<String> = None;
    if let Some(cfg) = auth {
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
        bearer = token.as_ref().map(|t| t.access_token.clone());
    }

    let mut resp = send_request(client, base_url, req, vars, bearer.as_deref())
        .await
        .map_err(|e| format!("{e:#}"));

    if let (Ok(r), Some(cfg)) = (&resp, auth)
        && r.status == 401
        && let Ok(t) = fetch_token(client, cfg).await
    {
        bearer = Some(t.access_token.clone());
        token = Some(t);
        resp = send_request(client, base_url, req, vars, bearer.as_deref())
            .await
            .map_err(|e| format!("{e:#}"));
    }

    SendOutcome {
        result: resp,
        token,
    }
}
