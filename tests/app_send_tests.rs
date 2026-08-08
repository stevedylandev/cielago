//! End-to-end: TUI action → async send task → response/token state update.

use std::path::PathBuf;

use cielago::app::App;
use cielago::model::{Collection, KeyValueRow, Method, OAuthConfig, SavedRequest};
use cielago::store::AppConfig;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn app_with(base_url: String) -> App {
    let mut c = Collection::new("test");
    c.servers = vec![base_url];
    let mut req = SavedRequest::blank("get thing");
    req.method = Method::Get;
    req.path = "/things/1".into();
    req.headers
        .push(KeyValueRow::new("X-Request-Id", "{{uuid}}", true));
    c.requests = vec![req];
    App::new(
        c,
        PathBuf::from("/tmp/cielago-test.json"),
        AppConfig::default(),
    )
}

#[tokio::test]
async fn send_from_app_updates_response_pane() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/things/1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 1})))
        .mount(&server)
        .await;

    let mut app = app_with(server.uri());
    assert!(app.response.is_none());

    app.send_selected();
    assert!(app.sending);

    let outcome = app.rx.recv().await.expect("send outcome");
    app.handle_outcome(outcome);

    assert!(!app.sending);
    let resp = app.response.as_ref().expect("response recorded");
    assert_eq!(resp.status, 200);
    assert!(resp.body.contains("\"id\": 1"));
    assert!(app.status.contains("200"));

    // {{uuid}} was substituted in the outgoing header.
    let received = server.received_requests().await.unwrap();
    let id = received[0]
        .headers
        .get("x-request-id")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(uuid::Uuid::parse_str(id).is_ok(), "got {id}");
}

#[tokio::test]
async fn send_with_oauth_fetches_and_caches_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "cached-tok",
            "expires_in": 3600
        })))
        .expect(1) // fetched once, then cached
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/things/1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(2)
        .mount(&server)
        .await;

    let mut app = app_with(server.uri());
    app.collection.auth = Some(OAuthConfig {
        token_url: format!("{}/token", server.uri()),
        client_id: "id".into(),
        client_secret: "secret".into(),
        scopes: vec![],
        auth_style: cielago::model::AuthStyle::Basic,
        ..Default::default()
    });

    // First send: fetches a token.
    app.send_selected();
    let outcome = app.rx.recv().await.unwrap();
    app.handle_outcome(outcome);
    assert_eq!(app.response.as_ref().unwrap().status, 200);
    assert!(app.token.is_some());

    // Second send: reuses the cached token (token endpoint expect(1)).
    app.send_selected();
    let outcome = app.rx.recv().await.unwrap();
    app.handle_outcome(outcome);
    assert_eq!(app.response.as_ref().unwrap().status, 200);

    // Both API calls carried the bearer token.
    let received = server.received_requests().await.unwrap();
    let api_calls: Vec<_> = received
        .iter()
        .filter(|r| r.url.path() == "/things/1")
        .collect();
    assert_eq!(api_calls.len(), 2);
    for r in api_calls {
        assert_eq!(
            r.headers.get("authorization").unwrap().to_str().unwrap(),
            "Bearer cached-tok"
        );
    }
}

#[tokio::test]
async fn send_without_server_shows_status_error() {
    let mut app = app_with("".into());
    app.collection.servers.clear();
    app.send_selected();
    assert!(!app.sending);
    assert!(app.status.contains("No server configured"));
}
