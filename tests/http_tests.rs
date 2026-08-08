use std::collections::HashMap;

use cielago::http::{fetch_token, send_request, send_with_auth};
use cielago::model::{AuthKind, AuthStyle, KeyValueRow, Method, OAuthConfig, SavedRequest};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn sends_request_with_params_and_uuid_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pets/123"))
        .and(query_param("limit", "10"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&server)
        .await;

    let mut req = SavedRequest::blank("get pet");
    req.method = Method::Get;
    req.path = "/pets/{petId}".into();
    req.path_params.push(KeyValueRow::new("petId", "123", true));
    req.query.push(KeyValueRow::new("limit", "10", true));
    req.query.push(KeyValueRow::new("disabled", "x", false));
    req.headers
        .push(KeyValueRow::new("X-Request-Id", "{{uuid}}", true));
    req.headers
        .push(KeyValueRow::new("X-Tenant", "{{tenant}}", true));

    let vars = HashMap::from([("tenant".to_string(), "acme".to_string())]);
    let client = reqwest::Client::new();
    let resp = send_request(&client, &server.uri(), &req, &vars, None, &[])
        .await
        .unwrap();

    assert_eq!(resp.status, 200);
    assert!(resp.body.contains("\"ok\": true"));

    // Inspect the recorded request.
    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let r = &received[0];
    // {{uuid}} became a real UUID.
    let id = r.headers.get("x-request-id").unwrap().to_str().unwrap();
    assert!(uuid::Uuid::parse_str(id).is_ok(), "got {id}");
    // {{tenant}} became acme.
    assert_eq!(r.headers.get("x-tenant").unwrap().to_str().unwrap(), "acme");
    // disabled query param was not sent.
    assert!(!r.url.query().unwrap_or_default().contains("disabled"));
}

#[tokio::test]
async fn oauth_client_credentials_basic_flow() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "tok-abc",
            "expires_in": 3600
        })))
        .mount(&server)
        .await;

    let cfg = OAuthConfig {
        token_url: format!("{}/token", server.uri()),
        client_id: "my-id".into(),
        client_secret: "my-secret".into(),
        scopes: vec!["read".into(), "write".into()],
        auth_style: AuthStyle::Basic,
        ..Default::default()
    };
    let client = reqwest::Client::new();
    let token = fetch_token(&client, &cfg).await.unwrap();
    assert_eq!(token.access_token, "tok-abc");
    assert!(cielago::http::token_valid(&token));

    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let r = &received[0];
    let auth = r
        .headers
        .get("authorization")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(auth.starts_with("Basic "), "got {auth}");
    let body = String::from_utf8_lossy(&r.body).into_owned();
    assert!(
        body.contains("grant_type=client_credentials"),
        "body: {body}"
    );
    assert!(body.contains("scope="), "body: {body}");
}

#[tokio::test]
async fn oauth_post_style_sends_creds_in_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "tok"
        })))
        .mount(&server)
        .await;

    let cfg = OAuthConfig {
        token_url: format!("{}/token", server.uri()),
        client_id: "id2".into(),
        client_secret: "secret2".into(),
        scopes: vec![],
        auth_style: AuthStyle::Post,
        ..Default::default()
    };
    let client = reqwest::Client::new();
    fetch_token(&client, &cfg).await.unwrap();

    let received = server.received_requests().await.unwrap();
    let r = &received[0];
    let body = String::from_utf8_lossy(&r.body).into_owned();
    assert!(body.contains("client_id=id2"), "body: {body}");
    assert!(body.contains("client_secret=secret2"), "body: {body}");
    assert!(r.headers.get("authorization").is_none());
}

#[tokio::test]
async fn bearer_token_injected_unless_header_present() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let req = SavedRequest::blank("x");
    send_request(
        &client,
        &server.uri(),
        &req,
        &HashMap::new(),
        Some("tok-1"),
        &[],
    )
    .await
    .unwrap();
    let received = server.received_requests().await.unwrap();
    assert_eq!(
        received[0]
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap(),
        "Bearer tok-1"
    );

    // Explicit Authorization header wins over the injected bearer.
    let mut req2 = SavedRequest::blank("x2");
    req2.headers
        .push(KeyValueRow::new("Authorization", "Bearer manual", true));
    send_request(
        &client,
        &server.uri(),
        &req2,
        &HashMap::new(),
        Some("tok-2"),
        &[],
    )
    .await
    .unwrap();
    let received = server.received_requests().await.unwrap();
    assert_eq!(
        received[1]
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap(),
        "Bearer manual"
    );
}

#[tokio::test]
async fn api_key_auth_resolves_shell_secret_into_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    // Value is a `$(…)` command substitution, resolved at send time.
    let cfg = OAuthConfig {
        kind: AuthKind::ApiKey,
        token: "$(printf 'sk-secret')".into(),
        header: "X-Api-Key".into(),
        ..Default::default()
    };
    let req = SavedRequest::blank("x");
    let client = reqwest::Client::new();
    let outcome = send_with_auth(
        &client,
        &server.uri(),
        &req,
        &HashMap::new(),
        Some(&cfg),
        None,
    )
    .await;
    assert!(outcome.result.is_ok(), "{:?}", outcome.result.err());
    assert!(outcome.token.is_none());

    let received = server.received_requests().await.unwrap();
    assert_eq!(
        received[0]
            .headers
            .get("x-api-key")
            .unwrap()
            .to_str()
            .unwrap(),
        "sk-secret"
    );
}

#[tokio::test]
async fn bearer_auth_sends_resolved_token() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let cfg = OAuthConfig {
        kind: AuthKind::Bearer,
        token: "plain-tok".into(),
        ..Default::default()
    };
    let req = SavedRequest::blank("x");
    let client = reqwest::Client::new();
    let outcome = send_with_auth(
        &client,
        &server.uri(),
        &req,
        &HashMap::new(),
        Some(&cfg),
        None,
    )
    .await;
    assert!(outcome.result.is_ok(), "{:?}", outcome.result.err());

    let received = server.received_requests().await.unwrap();
    assert_eq!(
        received[0]
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap(),
        "Bearer plain-tok"
    );
}

/// The compose/decompose contract: what `split_url_input` pulls apart,
/// `build_url` must put back together.
#[test]
fn pasted_url_round_trips_through_build_url() {
    let pasted = "https://api.example.com/orgs/{orgId}/pets?limit=10&sort=name";
    let parts = cielago::http::split_url_input(pasted);

    let mut req = SavedRequest::blank("round trip");
    req.path = parts.path;
    req.query = parts.query.unwrap();
    req.sync_path_params();
    // Path params come out of the paste blank; fill the one placeholder.
    assert_eq!(req.path_params.len(), 1);
    req.path_params[0].value = "acme".into();

    let base = parts.origin.unwrap();
    let url = cielago::http::client::build_url(&base, &req, &HashMap::new());
    assert_eq!(url, "https://api.example.com/orgs/acme/pets");
    // The query is applied by reqwest rather than `build_url`, so check the rows.
    let query: Vec<(&str, &str)> = req
        .query
        .iter()
        .map(|r| (r.key.as_str(), r.value.as_str()))
        .collect();
    assert_eq!(query, vec![("limit", "10"), ("sort", "name")]);
}

/// End to end: a pasted URL becomes a request that actually reaches the server
/// it named, with the query it carried.
#[tokio::test]
async fn a_pasted_url_sends_to_the_pasted_server() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/pets"))
        .and(query_param("limit", "5"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let parts = cielago::http::split_url_input(&format!("{}/v1/pets?limit=5", server.uri()));
    let mut req = SavedRequest::blank("pasted");
    req.path = parts.path;
    req.query = parts.query.unwrap();
    req.sync_path_params();

    let client = reqwest::Client::new();
    let resp = send_request(
        &client,
        &parts.origin.unwrap(),
        &req,
        &HashMap::new(),
        None,
        &[],
    )
    .await
    .unwrap();
    assert_eq!(resp.status, 200);
}
