use cielago::model::{Method, variables_map};
use cielago::openapi::{import_spec, load_spec};
use cielago::store;

fn fixture_path(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

async fn import_fixture(name: &str, coll_name: &str) -> cielago::model::Collection {
    let doc = load_spec(&fixture_path(name)).await.unwrap();
    import_spec(&doc, coll_name, Some(fixture_path(name)))
}

#[tokio::test]
async fn imports_petstore_30() {
    let c = import_fixture("petstore30.yaml", "pets").await;

    assert_eq!(
        c.servers,
        vec![
            "https://api.pets.example.com/v1".to_string(),
            "https://staging.pets.example.com/v1".to_string()
        ]
    );

    // OAuth clientCredentials flow is detected and prefilled.
    let auth = c.auth.as_ref().expect("auth should be prefilled");
    assert_eq!(auth.token_url, "https://auth.pets.example.com/oauth/token");
    assert_eq!(auth.scopes, vec!["read:pets", "write:pets"]);
    assert!(auth.client_id.is_empty());

    assert_eq!(c.requests.len(), 4);

    let list = c.requests.iter().find(|r| r.name == "listPets").unwrap();
    assert_eq!(list.method, Method::Get);
    assert_eq!(list.path, "/pets");
    assert_eq!(list.tags, vec!["pets"]);
    // Optional query params are populated but disabled; defaults prefilled.
    let limit = list.query.iter().find(|q| q.key == "limit").unwrap();
    assert!(!limit.enabled);
    assert_eq!(limit.value, "20");
    let filter = list.query.iter().find(|q| q.key == "filter").unwrap();
    assert!(!filter.enabled);
    assert_eq!(filter.value, "");
    // Required header param is enabled with its example.
    let tenant = list
        .headers
        .iter()
        .find(|h| h.key == "X-Tenant-Id")
        .unwrap();
    assert!(tenant.enabled);
    assert_eq!(tenant.value, "acme");

    // Authored media-type example wins for the body.
    let create = c.requests.iter().find(|r| r.name == "createPet").unwrap();
    assert_eq!(create.method, Method::Post);
    let body = create.body.as_deref().unwrap();
    assert!(body.contains("\"name\": \"Fido\""), "body was: {body}");

    // $ref'd path parameter is resolved and its example prefilled.
    let get_pet = c.requests.iter().find(|r| r.name == "getPet").unwrap();
    assert_eq!(get_pet.path, "/pets/{petId}");
    let pet_id = get_pet
        .path_params
        .iter()
        .find(|p| p.key == "petId")
        .unwrap();
    assert!(pet_id.enabled);
    assert_eq!(pet_id.value, "123");

    // Summary used as name when operationId is absent; body generated from
    // schema, uuid format becomes the {{uuid}} variable.
    let order = c.requests.iter().find(|r| r.name == "Place order").unwrap();
    assert_eq!(order.tags, vec!["store"]);
    let body = order.body.as_deref().unwrap();
    assert!(body.contains("\"petId\": 1"), "body was: {body}");
    assert!(
        body.contains("\"requestId\": \"{{uuid}}\""),
        "body was: {body}"
    );
}

#[tokio::test]
async fn import_captures_docs_for_the_docs_tab() {
    let c = import_fixture("petstore30.yaml", "pets docs").await;

    let list = c.requests.iter().find(|r| r.name == "listPets").unwrap();
    assert_eq!(
        list.description.as_deref(),
        Some("Lists pets, newest first.")
    );

    let limit = list.docs.iter().find(|d| d.name == "limit").unwrap();
    assert_eq!(limit.location, "query");
    assert_eq!(limit.ty, "integer");
    assert!(!limit.required);
    assert_eq!(limit.default.as_deref(), Some("20"));
    assert_eq!(
        limit.description.as_deref(),
        Some("How many pets to return.")
    );

    // The options a field accepts are what the tab is for.
    let status = list.docs.iter().find(|d| d.name == "status").unwrap();
    assert_eq!(status.options, ["available", "pending", "sold"]);

    let tenant = list.docs.iter().find(|d| d.name == "X-Tenant-Id").unwrap();
    assert_eq!(tenant.location, "header");
    assert!(tenant.required);

    // $ref'd path parameter, documented through the reference.
    let get_pet = c.requests.iter().find(|r| r.name == "getPet").unwrap();
    let pet_id = get_pet.docs.iter().find(|d| d.name == "petId").unwrap();
    assert_eq!(pet_id.location, "path");
    assert_eq!(pet_id.ty, "integer(int64)");
    assert!(pet_id.required);

    // Body fields come from the request body schema, `required` included.
    let create = c.requests.iter().find(|r| r.name == "createPet").unwrap();
    let body: Vec<(&str, &str, bool)> = create
        .docs
        .iter()
        .filter(|d| d.location == "body")
        .map(|d| (d.name.as_str(), d.ty.as_str(), d.required))
        .collect();
    assert_eq!(
        body,
        [
            ("id", "integer(int64)", false),
            ("name", "string", true),
            ("tag", "string", false)
        ]
    );
    assert_eq!(
        create
            .docs
            .iter()
            .find(|d| d.name == "tag")
            .unwrap()
            .default
            .as_deref(),
        Some("friendly")
    );

    // Hand-made requests simply have none.
    assert!(cielago::model::SavedRequest::blank("x").docs.is_empty());
}

#[tokio::test]
async fn imports_31_json() {
    let c = import_fixture("api31.json", "things").await;
    assert_eq!(c.servers, vec!["https://things.example.com".to_string()]);
    assert_eq!(c.requests.len(), 1);
    let make = &c.requests[0];
    assert_eq!(make.name, "makeThing");
    let body = make.body.as_deref().unwrap();
    assert!(body.contains("\"label\": \"widget\""), "body was: {body}");
    assert!(body.contains("\"count\": 1"), "body was: {body}");
}

#[test]
fn summary_wins_over_operation_id_for_naming() {
    let doc = serde_json::json!({
        "paths": {
            "/v1/customers/{id}": {
                "get": {
                    "operationId": "CustomerControllerV1_retrieveCustomerById",
                    "summary": "Get customer",
                    "tags": ["customers"]
                }
            },
            "/v1/health": { "get": { "operationId": "healthCheck" } },
            "/v1/ping": { "get": {} }
        }
    });
    let c = import_spec(&doc, "svc", None);

    let cust = c
        .requests
        .iter()
        .find(|r| r.path.contains("customers"))
        .unwrap();
    assert_eq!(cust.name, "Get customer");
    assert_eq!(cust.summary.as_deref(), Some("Get customer"));
    assert_eq!(
        cust.operation_id.as_deref(),
        Some("CustomerControllerV1_retrieveCustomerById")
    );

    // operationId is the fallback when there's no summary.
    let health = c.requests.iter().find(|r| r.path == "/v1/health").unwrap();
    assert_eq!(health.name, "healthCheck");
    assert_eq!(health.summary, None);

    // Neither present: METHOD + path.
    let ping = c.requests.iter().find(|r| r.path == "/v1/ping").unwrap();
    assert_eq!(ping.name, "GET /v1/ping");
}

#[test]
fn label_mode_selects_the_displayed_text() {
    use cielago::model::LabelMode;

    let doc = serde_json::json!({
        "paths": {
            "/v1/customers/{id}": {
                "get": { "operationId": "CustomerControllerV1_get", "summary": "Get customer" }
            }
        }
    });
    let c = import_spec(&doc, "svc", None);
    let r = &c.requests[0];
    assert_eq!(r.label(LabelMode::Name), "Get customer");
    assert_eq!(r.label(LabelMode::Summary), "Get customer");
    assert_eq!(r.label(LabelMode::Path), "/v1/customers/{id}");

    // No summary: Summary mode falls back to the name rather than blanking.
    let mut bare = cielago::model::SavedRequest::blank("hand made");
    bare.path = "/thing".into();
    assert_eq!(bare.label(LabelMode::Summary), "hand made");
    assert_eq!(bare.label(LabelMode::Path), "/thing");
}

#[tokio::test]
async fn collection_survives_save_load_roundtrip() {
    let c = import_fixture("petstore30.yaml", "pets roundtrip").await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("coll.json");
    std::fs::write(&path, serde_json::to_string_pretty(&c).unwrap()).unwrap();
    let back = store::load_collection_path(&path.to_path_buf()).unwrap();
    assert_eq!(back.requests.len(), 4);
    assert_eq!(
        back.auth.unwrap().token_url,
        "https://auth.pets.example.com/oauth/token"
    );
}

#[test]
fn variables_map_respects_enabled() {
    let vars = vec![
        cielago::model::KeyValueRow::new("a", "1", true),
        cielago::model::KeyValueRow::new("b", "2", false),
        cielago::model::KeyValueRow::new("", "3", true),
    ];
    let map = variables_map(&vars);
    assert_eq!(map.get("a").unwrap(), "1");
    assert!(!map.contains_key("b"));
    assert!(!map.contains_key(""));
}
