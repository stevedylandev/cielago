//! Render tests: draw the whole UI into an in-memory terminal and inspect the
//! cells. These cover the syntax highlighting, which the keymap tests can't see.

use std::path::PathBuf;
use std::time::Duration;

use cielago::app::{App, EditorTab, Mode};
use cielago::http::HttpResponse;
use cielago::model::{Collection, FieldDoc, Method, SavedRequest};
use cielago::store::AppConfig;
use cielago::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;

fn test_app() -> App {
    let mut c = Collection::new("test");
    c.servers = vec!["https://one.example.com".into()];
    let mut create = SavedRequest::blank("createPet");
    create.method = Method::Post;
    create.path = "/pets".into();
    create.body =
        Some("{\n  \"name\": \"{{petName}}\",\n  \"legs\": 4,\n  \"good\": true\n}".into());
    c.requests = vec![create];
    App::new(
        c,
        PathBuf::from("/tmp/cielago-ui-test.json"),
        AppConfig::default(),
    )
}

fn render(app: &mut App, w: u16, h: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| ui::draw(f, app)).unwrap();
    terminal.backend().buffer().clone()
}

fn row_text(buf: &Buffer, y: u16) -> String {
    (0..buf.area.width)
        .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
        .collect()
}

/// Foreground colour of the first cell of `needle` on screen. Rows contain
/// multi-byte box-drawing characters, so the byte offset is converted to a
/// column (one character per cell).
fn fg_of(buf: &Buffer, needle: &str) -> Option<Color> {
    for y in 0..buf.area.height {
        let row = row_text(buf, y);
        if let Some(byte) = row.find(needle) {
            let x = row[..byte].chars().count() as u16;
            return buf.cell((x, y)).map(|c| c.fg);
        }
    }
    panic!("{needle:?} not on screen");
}

fn screen(buf: &Buffer) -> String {
    (0..buf.area.height)
        .map(|y| row_text(buf, y))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn body_view_is_syntax_highlighted() {
    let mut app = test_app();
    app.tab = EditorTab::Body;
    let buf = render(&mut app, 100, 40);

    assert_eq!(fg_of(&buf, "\"name\""), Some(Color::Cyan), "object key");
    assert_eq!(fg_of(&buf, "4"), Some(Color::Yellow), "number");
    assert_eq!(fg_of(&buf, "true"), Some(Color::Magenta), "literal");
    // Variables stand out from the string they sit in.
    assert_eq!(fg_of(&buf, "{{petName}}"), Some(Color::Magenta));
}

#[test]
fn body_falls_back_to_the_plain_textarea_while_editing() {
    let mut app = test_app();
    app.tab = EditorTab::Body;
    app.mode = Mode::Insert;
    let buf = render(&mut app, 100, 40);

    assert!(screen(&buf).contains("\"name\""));
    assert_eq!(fg_of(&buf, "\"name\""), Some(Color::Reset));
}

#[test]
fn response_view_is_syntax_highlighted() {
    let mut app = test_app();
    app.response = Some(HttpResponse {
        status: 200,
        reason: "OK".into(),
        elapsed: Duration::from_millis(12),
        headers: vec![("content-type".into(), "application/json".into())],
        body: "{\n  \"id\": \"{{notavar}}\",\n  \"count\": 7\n}".into(),
        size: 40,
    });
    let buf = render(&mut app, 100, 40);

    assert!(screen(&buf).contains("200 OK · 12ms"));
    assert_eq!(fg_of(&buf, "\"id\""), Some(Color::Cyan));
    assert_eq!(fg_of(&buf, "7"), Some(Color::Yellow));
    // Braces in a response are the server's bytes, not template syntax.
    assert_eq!(fg_of(&buf, "\"{{notavar}}\""), Some(Color::Green));
}

#[test]
fn xml_response_is_syntax_highlighted() {
    let mut app = test_app();
    app.response = Some(HttpResponse {
        status: 500,
        reason: "Internal Server Error".into(),
        elapsed: Duration::from_millis(3),
        headers: Vec::new(),
        body: "<error code=\"500\">boom</error>".into(),
        size: 30,
    });
    let buf = render(&mut app, 100, 40);

    assert_eq!(fg_of(&buf, "<error"), Some(Color::Blue));
    assert_eq!(fg_of(&buf, "code"), Some(Color::Cyan));
    assert_eq!(fg_of(&buf, "\"500\""), Some(Color::Green));
    assert_eq!(fg_of(&buf, "boom"), Some(Color::Reset));
}

#[test]
fn docs_tab_shows_types_options_and_defaults() {
    let mut app = test_app();
    let req = &mut app.collection.requests[0];
    req.description = Some("Adds a pet to the store.".into());
    req.docs = vec![
        FieldDoc {
            name: "status".into(),
            location: "query".into(),
            ty: "string".into(),
            required: true,
            options: vec!["available".into(), "pending".into(), "sold".into()],
            description: Some("Which pets to return.".into()),
            default: Some("available".into()),
        },
        FieldDoc {
            name: "pets[].tag".into(),
            location: "body".into(),
            ty: "array<string>".into(),
            ..FieldDoc::default()
        },
    ];
    app.tab = EditorTab::Docs;
    let buf = render(&mut app, 100, 40);
    let text = screen(&buf);

    assert!(text.contains("Adds a pet to the store."), "{text}");
    assert!(text.contains("Query params"), "{text}");
    assert!(text.contains("status*"), "required marker: {text}");
    assert!(
        text.contains("one of: available | pending | sold"),
        "enum options: {text}"
    );
    assert!(text.contains("= available"), "default: {text}");
    assert!(text.contains("Which pets to return."), "{text}");
    assert!(text.contains("Body"), "{text}");
    assert!(text.contains("pets[].tag"), "{text}");
    assert_eq!(fg_of(&buf, "array<string>"), Some(Color::Yellow));
}

#[test]
fn docs_tab_explains_itself_when_there_is_nothing_to_show() {
    let mut app = test_app();
    app.tab = EditorTab::Docs;
    let text = screen(&render(&mut app, 100, 40));
    assert!(text.contains("No spec docs for this request"), "{text}");
}

#[test]
fn renders_without_panicking_in_edge_cases() {
    // Tiny terminal, empty body, long body scrolled to the bottom, help popup.
    let mut app = test_app();
    app.tab = EditorTab::Body;
    render(&mut app, 20, 8);

    app.set_textarea_text("");
    render(&mut app, 100, 40);

    app.set_textarea_text(&(1..=200).map(|i| format!("[{i}]\n")).collect::<String>());
    app.textarea.move_cursor(tui_textarea::CursorMove::Bottom);
    render(&mut app, 100, 40);

    app.tab = EditorTab::Docs;
    app.docs_scroll = usize::MAX / 2;
    render(&mut app, 100, 40);
    render(&mut app, 20, 8);

    app.popup = cielago::app::Popup::Help;
    app.help_scroll = usize::MAX / 2;
    render(&mut app, 100, 40);
    render(&mut app, 30, 10);

    // A collection with no requests at all: the sidebar hands ratatui a
    // selected index on a zero-row list, and there is nothing to draw in the
    // URL bar or editor.
    let mut empty = App::new(
        Collection::new("empty"),
        PathBuf::from("/tmp/cielago-ui-empty.json"),
        AppConfig::default(),
    );
    render(&mut empty, 100, 40);
    render(&mut empty, 20, 8);
}

#[test]
fn url_edit_prompt_shows_in_the_status_bar() {
    let mut app = test_app();
    app.start_edit(cielago::app::EditTarget::Url);
    let buf = render(&mut app, 100, 40);
    assert!(screen(&buf).contains("url> /pets"));
}

#[test]
fn help_lists_duplicate_and_new_collection() {
    let mut app = test_app();
    app.popup = cielago::app::Popup::Help;
    let top = screen(&render(&mut app, 100, 60));
    assert!(top.contains("duplicate"));
    assert!(top.contains("edit URL"));

    // The commands live past the fold, so scroll to the bottom for those.
    app.help_scroll = usize::MAX / 2;
    let bottom = screen(&render(&mut app, 100, 60));
    assert!(bottom.contains(":new"));
    assert!(bottom.contains(":open"));
}
