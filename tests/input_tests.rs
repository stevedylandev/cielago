//! State-machine tests for the TUI: synthetic key events drive `input::handle_key`
//! directly (no terminal needed).

use std::path::PathBuf;

use cielago::app::{App, EditTarget, EditorTab, Focus, Mode, Popup, SidebarRow};
use cielago::input::handle_key;
use cielago::model::{Collection, KeyValueRow, LabelMode, Method, SavedRequest};
use cielago::store::AppConfig;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn char_key(c: char) -> KeyEvent {
    key(KeyCode::Char(c))
}

fn type_str(app: &mut App, s: &str) {
    for c in s.chars() {
        handle_key(app, char_key(c));
    }
}

fn test_collection() -> Collection {
    let mut c = Collection::new("test");
    c.servers = vec![
        "https://one.example.com".into(),
        "https://two.example.com".into(),
    ];

    let mut list = SavedRequest::blank("listPets");
    list.method = Method::Get;
    list.path = "/pets".into();
    list.tags = vec!["pets".into()];
    list.query.push(KeyValueRow::new("limit", "20", false));
    list.query.push(KeyValueRow::new("filter", "", false));

    let mut create = SavedRequest::blank("createPet");
    create.method = Method::Post;
    create.path = "/pets".into();
    create.tags = vec!["pets".into()];
    create.body = Some("{\n  \"name\": \"Fido\"\n}".into());

    let mut order = SavedRequest::blank("placeOrder");
    order.method = Method::Post;
    order.path = "/orders".into();
    order.tags = vec!["store".into()];

    c.requests = vec![list, create, order];
    c
}

fn test_app() -> App {
    App::new(
        test_collection(),
        PathBuf::from("/tmp/cielago-test.json"),
        AppConfig::default(),
    )
}

#[test]
fn startup_state() {
    let app = test_app();
    assert_eq!(app.mode, Mode::Normal);
    // sidebar: group header + 2 pets requests + group + 1 store request
    assert_eq!(app.sidebar_rows.len(), 5);
    // first request auto-selected, but focus starts on the sidebar
    assert_eq!(app.selected, Some(0));
    assert_eq!(app.focus, Focus::Sidebar);
}

#[test]
fn startup_restores_the_last_open_request() {
    let mut c = test_collection();
    c.last_request = Some(c.requests[2].id);
    let app = App::new(
        c,
        PathBuf::from("/tmp/cielago-test.json"),
        AppConfig::default(),
    );
    assert_eq!(app.selected, Some(2));
    // the sidebar cursor lands on it too, not back at the top
    assert_eq!(app.sidebar_rows[app.sidebar_sel], SidebarRow::Request(2));
    assert_eq!(app.focus, Focus::Sidebar);
}

#[test]
fn startup_expands_the_restored_request_group() {
    let mut c = test_collection();
    c.groups_collapsed = true;
    c.last_request = Some(c.requests[2].id);
    let app = App::new(
        c,
        PathBuf::from("/tmp/cielago-test.json"),
        AppConfig::default(),
    );
    // "store" is expanded so the restored row is visible; "pets" stays collapsed
    assert_eq!(app.selected, Some(2));
    assert_eq!(app.sidebar_rows[app.sidebar_sel], SidebarRow::Request(2));
    assert_eq!(app.sidebar_rows.len(), 3);
}

#[test]
fn startup_restores_the_focused_pane_and_tab() {
    let mut c = test_collection();
    c.last_request = Some(c.requests[1].id);
    c.last_focus = Some(Focus::Editor);
    c.last_tab = Some(EditorTab::Body);
    let app = App::new(
        c,
        PathBuf::from("/tmp/cielago-test.json"),
        AppConfig::default(),
    );
    assert_eq!(app.selected, Some(1));
    assert_eq!(app.focus, Focus::Editor);
    assert_eq!(app.tab, EditorTab::Body);
}

#[test]
fn startup_skips_a_saved_response_pane_with_no_response() {
    let mut c = test_collection();
    c.last_focus = Some(Focus::Response);
    c.last_tab = Some(EditorTab::Docs);
    let app = App::new(
        c,
        PathBuf::from("/tmp/cielago-test.json"),
        AppConfig::default(),
    );
    // responses aren't persisted, so pane 3 would be empty
    assert_eq!(app.focus, Focus::Editor);
    assert_eq!(app.tab, EditorTab::Docs);
}

// `record_view` rather than `save`: saving writes into the real
// `~/.config/cielago/collections`, which a test has no business touching.
#[test]
fn record_view_captures_request_pane_and_tab() {
    let mut app = test_app();
    handle_key(&mut app, char_key('j')); // onto the first request row
    handle_key(&mut app, char_key('j')); // onto the second
    handle_key(&mut app, key(KeyCode::Enter)); // open it
    handle_key(&mut app, char_key(']')); // Params -> Headers
    handle_key(&mut app, char_key(']')); // Headers -> Body
    handle_key(&mut app, char_key('3')); // response pane
    app.record_view();

    let id = app.collection.requests[1].id;
    assert_eq!(app.collection.last_request, Some(id));
    assert_eq!(app.collection.last_focus, Some(Focus::Response));
    assert_eq!(app.collection.last_tab, Some(EditorTab::Body));
}

#[test]
fn startup_ignores_a_stale_last_request() {
    let mut c = test_collection();
    c.last_request = Some(uuid::Uuid::new_v4());
    let app = App::new(
        c,
        PathBuf::from("/tmp/cielago-test.json"),
        AppConfig::default(),
    );
    // request is gone (re-imported spec, deleted operation): fall back to first
    assert_eq!(app.selected, Some(0));
}

#[test]
fn help_popup_opens_and_closes() {
    let mut app = test_app();
    handle_key(&mut app, char_key('?'));
    assert_eq!(app.popup, Popup::Help);
    handle_key(&mut app, key(KeyCode::Esc));
    assert_eq!(app.popup, Popup::None);
}

#[test]
fn z_toggles_pane_zoom_without_touching_focus() {
    let mut app = test_app();
    handle_key(&mut app, char_key('2'));
    handle_key(&mut app, char_key('z'));
    assert!(app.zoom);
    assert_eq!(app.focus, Focus::Editor);
    // Focus still moves while zoomed — it just picks the maximized pane.
    handle_key(&mut app, key(KeyCode::Tab));
    assert!(app.zoom);
    assert_eq!(app.focus, Focus::Response);
    handle_key(&mut app, char_key('z'));
    assert!(!app.zoom);
}

#[test]
fn sidebar_navigation_and_selection() {
    let mut app = test_app();
    handle_key(&mut app, char_key('1'));
    assert_eq!(app.focus, Focus::Sidebar);
    handle_key(&mut app, char_key('j'));
    assert_eq!(app.sidebar_sel, 1);
    handle_key(&mut app, char_key('j'));
    handle_key(&mut app, key(KeyCode::Enter));
    assert_eq!(app.selected, Some(1)); // createPet
    assert_eq!(app.focus, Focus::Editor);
    // body loaded into textarea
    assert!(app.textarea.lines().join("\n").contains("Fido"));
}

#[test]
fn sidebar_group_collapse() {
    let mut app = test_app();
    handle_key(&mut app, char_key('1'));
    // row 0 is the "pets" group
    handle_key(&mut app, key(KeyCode::Enter));
    assert!(app.collapsed.contains("pets"));
    assert_eq!(app.sidebar_rows.len(), 3); // pets group collapsed
    handle_key(&mut app, key(KeyCode::Enter));
    assert!(!app.collapsed.contains("pets"));
    assert_eq!(app.sidebar_rows.len(), 5);
}

#[test]
fn quit_guards_unsaved_changes() {
    let mut app = test_app();
    assert!(!app.dirty);
    handle_key(&mut app, char_key('q'));
    assert!(app.should_quit);

    let mut app = test_app();
    app.dirty = true;
    handle_key(&mut app, char_key('q'));
    assert!(!app.should_quit);
    assert!(app.status.contains("Unsaved"));
    // :q! forces
    handle_key(&mut app, char_key(':'));
    type_str(&mut app, "q!");
    handle_key(&mut app, key(KeyCode::Enter));
    assert!(app.should_quit);
}

#[test]
fn tab_cycling() {
    let mut app = test_app();
    assert_eq!(app.tab, EditorTab::Params);
    handle_key(&mut app, char_key(']'));
    assert_eq!(app.tab, EditorTab::Headers);
    handle_key(&mut app, char_key(']'));
    assert_eq!(app.tab, EditorTab::Body);
    handle_key(&mut app, char_key('['));
    assert_eq!(app.tab, EditorTab::Headers);
}

#[test]
fn tab_cycling_letter_aliases() {
    let mut app = test_app();
    assert_eq!(app.tab, EditorTab::Params);
    handle_key(&mut app, char_key('L'));
    assert_eq!(app.tab, EditorTab::Headers);
    handle_key(&mut app, char_key('L'));
    assert_eq!(app.tab, EditorTab::Body);
    handle_key(&mut app, char_key('H'));
    assert_eq!(app.tab, EditorTab::Headers);
    // wraps backwards past Params into Variables
    handle_key(&mut app, char_key('H'));
    handle_key(&mut app, char_key('H'));
    assert_eq!(app.tab, EditorTab::Variables);
}

#[test]
fn slash_filters_the_sidebar() {
    let mut app = test_app();
    handle_key(&mut app, char_key('/'));
    assert_eq!(app.mode, Mode::Search);
    assert_eq!(app.focus, Focus::Sidebar);

    type_str(&mut app, "order");
    // "store" group header + placeOrder only
    assert_eq!(app.sidebar_rows.len(), 2);
    // cursor parked on the match, not the group header
    assert_eq!(app.sidebar_rows[app.sidebar_sel], SidebarRow::Request(2));

    // Enter keeps the filter and returns to Normal.
    handle_key(&mut app, key(KeyCode::Enter));
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.filter, "order");
    handle_key(&mut app, key(KeyCode::Enter)); // open the match
    assert_eq!(app.selected, Some(2));

    // Esc in the sidebar clears the filter.
    handle_key(&mut app, char_key('1'));
    handle_key(&mut app, key(KeyCode::Esc));
    assert!(app.filter.is_empty());
    assert_eq!(app.sidebar_rows.len(), 5);
}

#[test]
fn search_matches_path_method_and_tag() {
    let mut app = test_app();

    handle_key(&mut app, char_key('/'));
    type_str(&mut app, "/pets");
    assert_eq!(app.sidebar_rows.len(), 3); // pets group + 2 requests
    handle_key(&mut app, key(KeyCode::Esc));
    assert!(app.filter.is_empty());

    handle_key(&mut app, char_key('/'));
    type_str(&mut app, "post");
    assert_eq!(app.sidebar_rows.len(), 4); // createPet + placeOrder, 2 groups
    handle_key(&mut app, key(KeyCode::Esc));

    handle_key(&mut app, char_key('/'));
    type_str(&mut app, "store");
    assert_eq!(app.sidebar_rows.len(), 2);

    // backspacing widens the match set again
    handle_key(&mut app, key(KeyCode::Backspace));
    handle_key(&mut app, key(KeyCode::Backspace));
    handle_key(&mut app, key(KeyCode::Backspace));
    handle_key(&mut app, key(KeyCode::Backspace));
    handle_key(&mut app, key(KeyCode::Backspace));
    assert_eq!(app.sidebar_rows.len(), 5);
}

#[test]
fn search_shows_matches_inside_collapsed_groups() {
    let mut app = test_app();
    app.collapsed.insert("pets".into());
    app.rebuild_sidebar();
    assert_eq!(app.sidebar_rows.len(), 3);

    handle_key(&mut app, char_key('/'));
    type_str(&mut app, "createPet");
    assert_eq!(app.sidebar_rows.len(), 2);
}

#[test]
fn label_mode_cycles_and_persists_on_the_collection() {
    let mut app = test_app();
    handle_key(&mut app, char_key('1'));
    assert_eq!(app.collection.label_mode, LabelMode::Name);
    handle_key(&mut app, char_key('t'));
    assert_eq!(app.collection.label_mode, LabelMode::Summary);
    handle_key(&mut app, char_key('t'));
    assert_eq!(app.collection.label_mode, LabelMode::Path);
    handle_key(&mut app, char_key('t'));
    assert_eq!(app.collection.label_mode, LabelMode::Name);
    assert!(app.dirty);
}

#[test]
fn label_command_sets_mode() {
    let mut app = test_app();
    handle_key(&mut app, char_key(':'));
    type_str(&mut app, "label path");
    handle_key(&mut app, key(KeyCode::Enter));
    assert_eq!(app.collection.label_mode, LabelMode::Path);

    handle_key(&mut app, char_key(':'));
    type_str(&mut app, "label nonsense");
    handle_key(&mut app, key(KeyCode::Enter));
    assert_eq!(app.collection.label_mode, LabelMode::Path);
    assert!(app.status.contains("Usage"));
}

#[test]
fn rename_all_rewrites_names_from_paths() {
    let mut app = test_app();
    handle_key(&mut app, char_key(':'));
    type_str(&mut app, "rename-all method-path");
    handle_key(&mut app, key(KeyCode::Enter));
    assert_eq!(app.collection.requests[0].name, "GET /pets");
    assert_eq!(app.collection.requests[1].name, "POST /pets");
    assert_eq!(app.collection.requests[2].name, "POST /orders");
    assert!(app.dirty);
}

#[test]
fn edit_query_value_inline() {
    let mut app = test_app();
    handle_key(&mut app, char_key('2'));
    assert_eq!(app.tab, EditorTab::Params);
    // row 0 = limit (value "20")
    handle_key(&mut app, char_key('i'));
    assert_eq!(app.mode, Mode::Insert);
    assert!(matches!(app.editing, Some(EditTarget::Cell { .. })));
    handle_key(&mut app, char_key('5'));
    handle_key(&mut app, key(KeyCode::Enter));
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.collection.requests[0].query[0].value, "205");
    assert!(app.dirty);
}

#[test]
fn space_toggles_row() {
    let mut app = test_app();
    handle_key(&mut app, char_key('2'));
    assert!(!app.collection.requests[0].query[0].enabled);
    handle_key(&mut app, char_key(' '));
    assert!(app.collection.requests[0].query[0].enabled);
}

#[test]
fn add_header_row_with_uuid_variable() {
    let mut app = test_app();
    handle_key(&mut app, char_key('2'));
    handle_key(&mut app, char_key(']')); // Headers tab
    assert_eq!(app.tab, EditorTab::Headers);
    handle_key(&mut app, char_key('a'));
    type_str(&mut app, "X-Request-Id");
    handle_key(&mut app, key(KeyCode::Enter)); // chains to value edit
    type_str(&mut app, "{{uuid}}");
    handle_key(&mut app, key(KeyCode::Enter));
    assert_eq!(app.mode, Mode::Normal);
    let headers = &app.collection.requests[0].headers;
    assert_eq!(headers.len(), 1);
    assert_eq!(headers[0].key, "X-Request-Id");
    assert_eq!(headers[0].value, "{{uuid}}");
    assert!(headers[0].enabled);
}

#[test]
fn body_textarea_editing() {
    let mut app = test_app();
    // select createPet (has a body)
    app.select_request(1);
    handle_key(&mut app, char_key(']'));
    handle_key(&mut app, char_key(']')); // Body tab
    assert_eq!(app.tab, EditorTab::Body);
    handle_key(&mut app, char_key('i'));
    assert_eq!(app.mode, Mode::Insert);
    handle_key(&mut app, key(KeyCode::Esc));
    assert_eq!(app.mode, Mode::Normal);
    assert!(
        app.collection.requests[1]
            .body
            .as_ref()
            .unwrap()
            .contains("Fido")
    );
}

#[test]
fn body_tab_scrolls_the_read_only_view() {
    let mut app = test_app();
    app.select_request(1);
    app.set_textarea_text(&(1..=40).map(|i| format!("line {i}\n")).collect::<String>());
    app.tab = EditorTab::Body;

    // The highlighted body view follows the textarea cursor.
    handle_key(&mut app, char_key('j'));
    handle_key(&mut app, char_key('j'));
    assert_eq!(app.textarea.cursor().0, 2);
    handle_key(&mut app, char_key('k'));
    assert_eq!(app.textarea.cursor().0, 1);
    handle_key(&mut app, char_key('d'));
    assert_eq!(app.textarea.cursor().0, 16);
    handle_key(&mut app, char_key('u'));
    assert_eq!(app.textarea.cursor().0, 1);
    handle_key(&mut app, char_key('G'));
    assert!(app.textarea.cursor().0 >= 39);
    handle_key(&mut app, char_key('g'));
    assert_eq!(app.textarea.cursor().0, 0);

    // `d` scrolls here rather than deleting a row, but the other editor keys
    // still reach their handlers.
    assert_eq!(app.collection.requests[1].method, Method::Post);
    handle_key(&mut app, char_key('m'));
    assert_eq!(app.collection.requests[1].method, Method::Put);
    handle_key(&mut app, char_key('i'));
    assert_eq!(app.mode, Mode::Insert);
}

#[test]
fn docs_tab_scrolls_and_stays_read_only() {
    let mut app = test_app();
    app.select_request(0); // moves focus to the editor
    app.tab = EditorTab::Docs;
    let params_before = app.collection.requests[0].query.len();

    handle_key(&mut app, char_key('j'));
    handle_key(&mut app, char_key('j'));
    assert_eq!(app.docs_scroll, 2);
    handle_key(&mut app, char_key('k'));
    assert_eq!(app.docs_scroll, 1);
    handle_key(&mut app, char_key('d'));
    assert_eq!(app.docs_scroll, 16);
    handle_key(&mut app, char_key('u'));
    assert_eq!(app.docs_scroll, 1);
    handle_key(&mut app, char_key('g'));
    assert_eq!(app.docs_scroll, 0);

    // `d` scrolled instead of deleting, and `i` doesn't open an editor here.
    assert_eq!(app.collection.requests[0].query.len(), params_before);
    handle_key(&mut app, char_key('i'));
    assert_eq!(app.mode, Mode::Normal);
    assert!(!app.dirty);

    // Opening another request resets the scroll.
    app.docs_scroll = 5;
    app.select_request(2);
    assert_eq!(app.docs_scroll, 0);
}

#[test]
fn help_popup_scrolls() {
    let mut app = test_app();
    handle_key(&mut app, char_key('?'));
    assert_eq!(app.help_scroll, 0);
    handle_key(&mut app, char_key('j'));
    handle_key(&mut app, char_key('j'));
    assert_eq!(app.help_scroll, 2);
    handle_key(&mut app, char_key('k'));
    assert_eq!(app.help_scroll, 1);
    handle_key(&mut app, char_key('g'));
    assert_eq!(app.help_scroll, 0);
    // Reopening starts back at the top.
    handle_key(&mut app, char_key('d'));
    assert!(app.help_scroll > 0);
    handle_key(&mut app, key(KeyCode::Esc));
    handle_key(&mut app, char_key('?'));
    assert_eq!(app.help_scroll, 0);
}

#[test]
fn env_popup_add_and_select_server() {
    let mut app = test_app();
    handle_key(&mut app, char_key('E'));
    assert_eq!(app.popup, Popup::Env);
    handle_key(&mut app, char_key('a'));
    type_str(&mut app, "http://localhost:8080");
    handle_key(&mut app, key(KeyCode::Enter));
    assert_eq!(app.collection.servers.len(), 3);
    assert_eq!(app.collection.active_server, 2);
    handle_key(&mut app, key(KeyCode::Esc));
    assert_eq!(app.popup, Popup::None);
}

#[test]
fn auth_popup_edits_and_applies() {
    let mut app = test_app();
    handle_key(&mut app, char_key('A'));
    assert_eq!(app.popup, Popup::Auth);
    // field 0 = token url
    handle_key(&mut app, char_key('i'));
    type_str(&mut app, "https://auth.example.com/token");
    handle_key(&mut app, key(KeyCode::Enter));
    // move to client id, edit
    handle_key(&mut app, char_key('j'));
    handle_key(&mut app, char_key('i'));
    type_str(&mut app, "my-client");
    handle_key(&mut app, key(KeyCode::Enter));
    // style toggle: field 4
    for _ in 0..3 {
        handle_key(&mut app, char_key('j'));
    }
    assert_eq!(app.auth_field, 4);
    handle_key(&mut app, char_key(' '));
    // close + apply
    handle_key(&mut app, key(KeyCode::Esc));
    assert_eq!(app.popup, Popup::None);
    let auth = app.collection.auth.as_ref().unwrap();
    assert_eq!(auth.token_url, "https://auth.example.com/token");
    assert_eq!(auth.client_id, "my-client");
    assert_eq!(auth.auth_style, cielago::model::AuthStyle::Post);
    assert!(app.dirty);
}

#[test]
fn new_request_flow() {
    let mut app = test_app();
    handle_key(&mut app, char_key('1'));
    handle_key(&mut app, char_key('n'));
    type_str(&mut app, "my custom request");
    handle_key(&mut app, key(KeyCode::Enter));
    assert_eq!(app.collection.requests.len(), 4);
    assert_eq!(app.selected, Some(3));
    assert_eq!(app.collection.requests[3].name, "my custom request");
}

#[test]
fn rename_and_delete_request() {
    let mut app = test_app();
    handle_key(&mut app, char_key('1'));
    handle_key(&mut app, char_key('j')); // first request row
    handle_key(&mut app, char_key('r'));
    // rename input prefilled with current name; replace
    handle_key(&mut app, key(KeyCode::Home));
    for _ in 0..20 {
        handle_key(&mut app, key(KeyCode::Delete));
    }
    type_str(&mut app, "renamed");
    handle_key(&mut app, key(KeyCode::Enter));
    assert_eq!(app.collection.requests[0].name, "renamed");

    // focus stays in the sidebar on the renamed row; delete it
    assert_eq!(app.focus, Focus::Sidebar);
    handle_key(&mut app, char_key('d'));
    assert_eq!(app.collection.requests.len(), 2);
    assert!(!app.collection.requests.iter().any(|r| r.name == "renamed"));
}

#[test]
fn variables_tab_roundtrip() {
    let mut app = test_app();
    handle_key(&mut app, char_key('2'));
    handle_key(&mut app, char_key('[')); // Variables (prev of Params)
    assert_eq!(app.tab, EditorTab::Variables);
    handle_key(&mut app, char_key('a'));
    type_str(&mut app, "tenant");
    handle_key(&mut app, key(KeyCode::Enter));
    type_str(&mut app, "acme");
    handle_key(&mut app, key(KeyCode::Enter));
    assert_eq!(app.collection.variables.len(), 1);
    let map = cielago::model::variables_map(&app.collection.variables);
    assert_eq!(map.get("tenant").unwrap(), "acme");
}

// ----- ad-hoc requests and collections -----

/// Open the URL prompt on the selected request with a cleared buffer.
fn start_url_edit(app: &mut App) {
    handle_key(app, char_key('2'));
    handle_key(app, char_key('p'));
    assert_eq!(app.editing, Some(EditTarget::Url));
    handle_key(app, key(KeyCode::Home));
    for _ in 0..60 {
        handle_key(app, key(KeyCode::Delete));
    }
}

fn type_url(app: &mut App, url: &str) {
    start_url_edit(app);
    type_str(app, url);
    handle_key(app, key(KeyCode::Enter));
}

#[test]
fn edit_url_sets_path_and_syncs_path_params() {
    let mut app = test_app();
    type_url(&mut app, "/pets/{petId}/photos");

    let req = &app.collection.requests[0];
    assert_eq!(req.path, "/pets/{petId}/photos");
    assert_eq!(req.path_params.len(), 1);
    assert_eq!(req.path_params[0].key, "petId");
    assert!(app.dirty);
    // No `?` in the input, so the existing query rows are untouched.
    assert_eq!(req.query.len(), 2);

    // Removing the placeholder prunes the row again.
    type_url(&mut app, "/pets");
    assert!(app.collection.requests[0].path_params.is_empty());
}

#[test]
fn pasting_a_full_url_adds_and_activates_the_server() {
    let mut app = test_app();
    type_url(
        &mut app,
        "https://three.example.com/v2/pets?limit=5&sort=name",
    );

    assert_eq!(app.collection.servers.len(), 3);
    assert_eq!(app.collection.active_server, 2);
    assert_eq!(app.collection.base_url(), Some("https://three.example.com"));
    let req = &app.collection.requests[0];
    assert_eq!(req.path, "/v2/pets");
    let query: Vec<(&str, &str)> = req
        .query
        .iter()
        .map(|r| (r.key.as_str(), r.value.as_str()))
        .collect();
    assert_eq!(query, vec![("limit", "5"), ("sort", "name")]);
    assert!(req.query.iter().all(|r| r.enabled));
}

#[test]
fn pasting_a_known_origin_switches_to_it_without_duplicating() {
    let mut app = test_app();
    assert_eq!(app.collection.active_server, 0);
    type_url(&mut app, "https://two.example.com/pets");

    assert_eq!(app.collection.servers.len(), 2);
    assert_eq!(app.collection.active_server, 1);
    assert_eq!(app.collection.requests[0].path, "/pets");
}

#[test]
fn pasting_without_a_query_keeps_existing_params() {
    let mut app = test_app();
    type_url(&mut app, "https://one.example.com/pets/all");

    let req = &app.collection.requests[0];
    assert_eq!(req.path, "/pets/all");
    assert_eq!(req.query.len(), 2);
    // Disabled optional params from the fixture survive untouched.
    assert!(req.query.iter().all(|r| !r.enabled));
    assert_eq!(req.query[0].key, "limit");
}

#[test]
fn non_http_scheme_is_rejected() {
    let mut app = test_app();
    type_url(&mut app, "ftp://files.example.com/pets");

    assert_eq!(app.collection.requests[0].path, "/pets");
    assert_eq!(app.collection.servers.len(), 2);
    assert!(app.status.contains("http(s)"));
}

#[test]
fn duplicate_request_clones_directly_after_the_original() {
    let mut app = test_app();
    handle_key(&mut app, char_key('1'));
    handle_key(&mut app, char_key('j')); // first request row (listPets)
    handle_key(&mut app, char_key('y'));

    assert_eq!(app.collection.requests.len(), 4);
    assert_eq!(app.collection.requests[1].name, "listPets copy");
    assert_ne!(app.collection.requests[0].id, app.collection.requests[1].id);
    assert_eq!(app.collection.requests[1].path, "/pets");
    assert_eq!(app.collection.requests[1].query.len(), 2);
    // The original is still in place, and the clone is what's open.
    assert_eq!(app.collection.requests[0].name, "listPets");
    assert_eq!(app.selected, Some(1));
    assert_eq!(app.focus, Focus::Sidebar);
    assert_eq!(app.sidebar_rows[app.sidebar_sel], SidebarRow::Request(1));
    assert!(app.dirty);
}

#[test]
fn duplicate_twice_numbers_the_copies() {
    let mut app = test_app();
    handle_key(&mut app, char_key('1'));
    handle_key(&mut app, char_key('j'));
    handle_key(&mut app, char_key('y'));
    handle_key(&mut app, char_key('y'));

    assert_eq!(app.collection.requests.len(), 5);
    let names: Vec<&str> = app
        .collection
        .requests
        .iter()
        .map(|r| r.name.as_str())
        .collect();
    // The cursor followed the first clone, so the second `y` duplicates *it* —
    // and the ` copy` suffix is stripped first, giving `copy 2` not `copy copy`.
    assert_eq!(names[1], "listPets copy");
    assert_eq!(names[2], "listPets copy 2");
}

#[test]
fn new_request_chains_into_the_url_prompt() {
    let mut app = test_app();
    handle_key(&mut app, char_key('1'));
    handle_key(&mut app, char_key('n'));
    type_str(&mut app, "adhoc");
    handle_key(&mut app, key(KeyCode::Enter));

    // The name commit leaves you in the URL prompt rather than on `GET /`.
    assert_eq!(app.editing, Some(EditTarget::Url));
    assert_eq!(app.mode, Mode::Insert);
    type_str(&mut app, "https://four.example.com/ip");
    handle_key(&mut app, key(KeyCode::Enter));

    let req = app.collection.requests.last().unwrap();
    assert_eq!(req.name, "adhoc");
    assert_eq!(req.path, "/ip");
    assert_eq!(app.collection.base_url(), Some("https://four.example.com"));
}

#[test]
fn switch_collection_replaces_state() {
    let mut app = test_app();
    handle_key(&mut app, char_key('/'));
    type_str(&mut app, "orders");
    handle_key(&mut app, key(KeyCode::Enter));
    handle_key(&mut app, char_key('2'));
    handle_key(&mut app, char_key('m')); // dirty it

    let mut other = Collection::new("other");
    other.requests = vec![SavedRequest::blank("only")];
    app.switch_collection(other, PathBuf::from("/tmp/cielago-other.json"));

    assert_eq!(app.collection.name, "other");
    assert_eq!(app.collection.requests.len(), 1);
    assert_eq!(app.selected, Some(0));
    assert!(!app.dirty);
    assert!(app.filter.is_empty());
    assert!(app.response.is_none());
    assert!(app.status.contains("other"));
    // Tracked in memory only — `switch_collection` must not write to the real
    // `~/.config/cielago`, which is where `store::config_dir` always points.
    assert_eq!(app.config.last_collection.as_deref(), Some("other"));
}

#[test]
fn new_collection_command_refuses_when_dirty() {
    let mut app = test_app();
    handle_key(&mut app, char_key('2'));
    handle_key(&mut app, char_key('m')); // cycle method → dirty
    assert!(app.dirty);

    // The dirty guard runs before any filesystem access, so this touches nothing.
    handle_key(&mut app, char_key(':'));
    type_str(&mut app, "new Scratch");
    handle_key(&mut app, key(KeyCode::Enter));

    assert_eq!(app.collection.name, "test");
    assert!(app.status.contains("Unsaved changes"));
}

#[test]
fn new_collection_command_rejects_a_missing_name() {
    let mut app = test_app();
    handle_key(&mut app, char_key(':'));
    type_str(&mut app, "new  ");
    handle_key(&mut app, key(KeyCode::Enter));

    assert_eq!(app.collection.name, "test");
    assert!(app.status.starts_with("Usage: :new"));
}
