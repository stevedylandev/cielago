//! Rendering: sidebar / URL bar / editor tabs / response / status / popups.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState,
    Tabs, Wrap,
};

use crate::app::{App, EditTarget, EditorTab, Focus, Mode, Popup, SidebarRow, TableId};
use crate::highlight;
use crate::http::DYNAMIC_VARS;
use crate::model::Method;

const SIDEBAR_WIDTH: u16 = 38;

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let [main_area, status_area] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(area);
    if app.zoom {
        // Only the focused pane is drawn, filling everything above the status
        // bar. The URL bar belongs to the editor pane (it renders the selected
        // request and takes the editor's focus colour), so it comes along.
        match app.focus {
            Focus::Sidebar => draw_sidebar(f, app, main_area),
            Focus::Editor => {
                let [url_area, editor_area] =
                    Layout::vertical([Constraint::Length(3), Constraint::Min(3)]).areas(main_area);
                draw_url_bar(f, app, url_area);
                draw_editor(f, app, editor_area);
            }
            Focus::Response => draw_response(f, app, main_area),
        }
    } else {
        let [side_area, right_area] =
            Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(40)])
                .areas(main_area);
        let [url_area, editor_area, response_area] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Percentage(45),
            Constraint::Min(5),
        ])
        .areas(right_area);

        draw_sidebar(f, app, side_area);
        draw_url_bar(f, app, url_area);
        draw_editor(f, app, editor_area);
        draw_response(f, app, response_area);
    }
    draw_status(f, app, status_area);

    match app.popup {
        Popup::Help => draw_help(f, app, area),
        Popup::Env => draw_env(f, app, area),
        Popup::Auth => draw_auth(f, app, area),
        Popup::None => {}
    }
}

// ----- shared styles -----

fn focused(focus: bool) -> Style {
    if focus {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn method_color(m: Method) -> Color {
    match m {
        Method::Get => Color::Green,
        Method::Post => Color::Yellow,
        Method::Put => Color::Blue,
        Method::Patch => Color::Magenta,
        Method::Delete => Color::Red,
        Method::Head => Color::Cyan,
        Method::Options => Color::Gray,
    }
}

fn checkbox(enabled: bool) -> &'static str {
    if enabled { "[x]" } else { "[ ]" }
}

// ----- sidebar -----

fn draw_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    let title = if app.filter.is_empty() {
        format!(" {} ", app.collection.name)
    } else {
        format!(" {} — /{} ", app.collection.name, app.filter)
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(focused(app.focus == Focus::Sidebar));

    let mut items: Vec<ListItem> = Vec::new();
    for row in &app.sidebar_rows {
        match row {
            SidebarRow::Group(tag) => {
                let marker = if app.collapsed.contains(tag) {
                    "▸"
                } else {
                    "▾"
                };
                items.push(ListItem::new(Line::from(vec![Span::styled(
                    format!("{marker} {tag}"),
                    Style::default().add_modifier(Modifier::BOLD),
                )])));
            }
            SidebarRow::Request(i) => {
                let req = &app.collection.requests[*i];
                items.push(ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("  {:<7}", req.method.to_string()),
                        Style::default().fg(method_color(req.method)),
                    ),
                    Span::raw(req.label(app.collection.label_mode).to_string()),
                ])));
            }
        }
    }

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">");
    // Keep the selection centered while scrolling; pin to the ends near top/bottom.
    let viewport = area.height.saturating_sub(2) as usize; // minus borders
    let offset = centered_offset(app.sidebar_sel, app.sidebar_rows.len(), viewport);
    let mut state = ListState::default()
        .with_selected(Some(app.sidebar_sel))
        .with_offset(offset);
    f.render_stateful_widget(list, area, &mut state);
}

/// Scroll offset that holds `sel` at the vertical middle of a `viewport`-tall
/// list, clamped so the first and last items never scroll past the edges.
fn centered_offset(sel: usize, len: usize, viewport: usize) -> usize {
    if viewport == 0 || len <= viewport {
        return 0;
    }
    let max_offset = len - viewport;
    sel.saturating_sub(viewport / 2).min(max_offset)
}

// ----- URL bar -----

fn draw_url_bar(f: &mut Frame, app: &App, area: Rect) {
    let (title, line) = match app.selected_request() {
        Some(req) => {
            let url = format!(
                "{}{}",
                app.collection.base_url().unwrap_or("<no server — press E>"),
                req.path
            );
            (
                format!(" {} — p: edit url ", req.name),
                Line::from(vec![
                    Span::styled(
                        format!(" {:<7}", req.method.to_string()),
                        Style::default()
                            .fg(method_color(req.method))
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(url),
                ]),
            )
        }
        None => (
            " cielago ".to_string(),
            Line::from("No request selected — pick one from the sidebar"),
        ),
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(focused(app.focus == Focus::Editor));
    f.render_widget(Paragraph::new(line).block(block), area);
}

// ----- editor -----

fn draw_editor(f: &mut Frame, app: &mut App, area: Rect) {
    let outer = Block::default()
        .borders(Borders::ALL)
        .border_style(focused(app.focus == Focus::Editor));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let [tabs_area, content_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);

    let tabs = Tabs::new(EditorTab::ALL.iter().map(|t| t.title()).collect::<Vec<_>>())
        .select(app.tab.index())
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )
        .divider("│");
    f.render_widget(tabs, tabs_area);

    match app.tab {
        EditorTab::Body => draw_body(f, app, content_area),
        EditorTab::Docs => draw_docs(f, app, content_area),
        tab => {
            if let Some(table) = tab.table() {
                draw_table(f, app, content_area, table)
            }
        }
    }
}

/// The body is syntax-highlighted and read-only; edits go through `$EDITOR`
/// (`e`). `body_scroll` is a plain offset, clamped here against the content.
fn draw_body(f: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .title(" Body — e: $EDITOR · j/k: scroll ")
        .borders(Borders::NONE);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines = highlight::highlight(&app.body_text, true);
    let max = lines.len().saturating_sub(1);
    app.body_scroll = app.body_scroll.min(max);
    f.render_widget(
        Paragraph::new(lines).scroll((app.body_scroll as u16, 0)),
        inner,
    );
}

/// Read-only view of what the spec says about this request: the operation
/// description, then every parameter and body field with its type, accepted
/// values and default.
fn draw_docs(f: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .title(" Docs — j/k: scroll · read-only ")
        .borders(Borders::NONE);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    match app.selected_request() {
        None => lines.push(Line::raw("No request selected.")),
        Some(req) => {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{} {}", req.method, req.path),
                    Style::default()
                        .fg(method_color(req.method))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    req.summary.clone().unwrap_or_else(|| req.name.clone()),
                    Style::default().fg(Color::Gray),
                ),
            ]));
            if let Some(desc) = &req.description {
                lines.push(Line::raw(""));
                lines.extend(
                    desc.lines()
                        .map(|l| Line::styled(l.to_string(), Style::default().fg(Color::Gray))),
                );
            }

            if req.docs.is_empty() {
                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    "No spec docs for this request. Hand-made requests have none;",
                    Style::default().fg(Color::DarkGray),
                ));
                lines.push(Line::styled(
                    "for imported ones, re-import the spec to fill this in.",
                    Style::default().fg(Color::DarkGray),
                ));
            }
            for (location, heading) in [
                ("path", "Path params"),
                ("query", "Query params"),
                ("header", "Headers"),
                ("body", "Body"),
                ("response", "Response"),
            ] {
                let fields = req.docs.iter().filter(|d| d.location == location);
                let mut first = true;
                for d in fields {
                    if first {
                        lines.push(Line::raw(""));
                        lines.push(Line::styled(
                            heading.to_string(),
                            Style::default().add_modifier(Modifier::BOLD),
                        ));
                        first = false;
                    }
                    let mut head = vec![
                        Span::styled(format!("  {}", d.name), Style::default().fg(Color::Cyan)),
                        // Required fields are starred, as in most API docs.
                        Span::styled(
                            if d.required { "*" } else { "" },
                            Style::default().fg(Color::Red),
                        ),
                        Span::raw("  "),
                        Span::styled(d.ty.clone(), Style::default().fg(Color::Yellow)),
                    ];
                    if let Some(default) = &d.default {
                        head.push(Span::styled(
                            format!("  = {default}"),
                            Style::default().fg(Color::DarkGray),
                        ));
                    }
                    lines.push(Line::from(head));
                    if !d.options.is_empty() {
                        lines.push(Line::from(vec![
                            Span::styled("    one of: ", Style::default().fg(Color::DarkGray)),
                            Span::styled(
                                d.options.join(" | "),
                                Style::default().fg(Color::Magenta),
                            ),
                        ]));
                    }
                    if let Some(desc) = &d.description {
                        lines.extend(desc.lines().map(|l| {
                            Line::styled(format!("    {l}"), Style::default().fg(Color::Gray))
                        }));
                    }
                }
            }
        }
    }

    // Wrapping means this is a lower bound on the rendered height, so the last
    // line always stays reachable.
    let max_scroll = lines.len().saturating_sub(inner.height as usize);
    app.docs_scroll = app.docs_scroll.min(max_scroll);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.docs_scroll as u16, 0)),
        inner,
    );
}

fn draw_table(f: &mut Frame, app: &App, area: Rect, table: TableId) {
    let rows_data: Vec<(bool, String, String, String)> = match table {
        TableId::Params => {
            let mut v: Vec<(bool, String, String, String)> = Vec::new();
            if let Some(req) = app.selected_request() {
                v.extend(
                    req.path_params
                        .iter()
                        .map(|r| (r.enabled, "path".into(), r.key.clone(), r.value.clone())),
                );
                v.extend(
                    req.query
                        .iter()
                        .map(|r| (r.enabled, "query".into(), r.key.clone(), r.value.clone())),
                );
            }
            v
        }
        TableId::Headers => app
            .selected_request()
            .map(|req| {
                req.headers
                    .iter()
                    .map(|r| (r.enabled, String::new(), r.key.clone(), r.value.clone()))
                    .collect()
            })
            .unwrap_or_default(),
        TableId::Vars => app
            .collection
            .variables
            .iter()
            .map(|r| (r.enabled, String::new(), r.key.clone(), r.value.clone()))
            .collect(),
    };

    let rows: Vec<Row> = rows_data
        .iter()
        .map(|(enabled, loc, key, value)| {
            let style = if *enabled {
                Style::default()
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let mut cells = vec![Cell::from(checkbox(*enabled))];
            if table == TableId::Params {
                cells.push(Cell::from(loc.clone()));
            }
            cells.push(Cell::from(key.clone()));
            cells.push(Cell::from(value.clone()));
            Row::new(cells).style(style)
        })
        .collect();

    let widths: Vec<Constraint> = if table == TableId::Params {
        vec![
            Constraint::Length(4),
            Constraint::Length(6),
            Constraint::Percentage(30),
            Constraint::Min(10),
        ]
    } else {
        vec![
            Constraint::Length(4),
            Constraint::Percentage(30),
            Constraint::Min(10),
        ]
    };

    let hint = match table {
        TableId::Vars => {
            " Variables — {{name}} usable anywhere · space: toggle · a: add · i: edit · d: del "
        }
        _ => " space: toggle · a: add · i: edit · d: del · Enter: send ",
    };

    let t = Table::new(rows, widths)
        .block(Block::default().title(hint).borders(Borders::NONE))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">");
    let mut state = TableState::default().with_selected(if rows_data.is_empty() {
        None
    } else {
        Some(app.table_row)
    });
    f.render_stateful_widget(t, area, &mut state);
}

// ----- response -----

fn draw_response(f: &mut Frame, app: &mut App, area: Rect) {
    let title = match (&app.response, app.sending) {
        (_, true) => " Response — sending… ".to_string(),
        (Some(resp), false) => format!(" Response — {} ", resp.status_line()),
        (None, false) => " Response ".to_string(),
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(focused(app.focus == Focus::Response));

    let body = app
        .response
        .as_ref()
        .map(|r| r.body.as_str())
        .unwrap_or("No response yet — press Enter on the editor to send.");

    // Clamp scroll to content length.
    let max_scroll = body.lines().count().saturating_sub(1);
    if app.response_scroll > max_scroll {
        app.response_scroll = max_scroll;
    }

    // `{{…}}` in a response is literal server output, not template syntax.
    let p = Paragraph::new(highlight::highlight(body, false))
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.response_scroll as u16, 0));
    f.render_widget(p, area);
}

// ----- status bar -----

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    match app.mode {
        Mode::Command => {
            f.render_widget(Paragraph::new(format!(":{}", app.command)), area);
            f.set_cursor_position((area.x + 1 + app.command.len() as u16, area.y));
        }
        Mode::Search => {
            f.render_widget(Paragraph::new(format!("/{}", app.search.buf)), area);
            let cursor_chars = app.search.buf[..app.search.cursor].chars().count() as u16;
            f.set_cursor_position((area.x + 1 + cursor_chars, area.y));
        }
        Mode::Insert if app.editing.is_some() => {
            let label = match app.editing.unwrap() {
                EditTarget::Cell { col, .. } => match col {
                    crate::app::CellCol::Key => "key",
                    crate::app::CellCol::Value => "value",
                },
                EditTarget::Rename => "rename",
                EditTarget::NewRequest => "new request",
                EditTarget::Url => "url (verb path)",
                EditTarget::EnvNew => "server url",
                EditTarget::AuthField(_) => "auth",
            };
            let prompt = format!("{label}> ");
            f.render_widget(Paragraph::new(format!("{prompt}{}", app.input.buf)), area);
            let cursor_chars = app.input.buf[..app.input.cursor].chars().count() as u16;
            f.set_cursor_position((area.x + prompt.len() as u16 + cursor_chars, area.y));
        }
        _ => {
            let mode_badge = match app.mode {
                Mode::Normal => Span::styled(
                    " NORMAL ",
                    Style::default().bg(Color::Green).fg(Color::Black),
                ),
                Mode::Insert => Span::styled(
                    " INSERT ",
                    Style::default().bg(Color::Yellow).fg(Color::Black),
                ),
                Mode::Command | Mode::Search => Span::raw(""),
            };
            let dirty = if app.dirty { "*" } else { "" };
            let server = app.collection.base_url().unwrap_or("no server");
            let line = Line::from(vec![
                mode_badge,
                Span::raw(format!(
                    " {}{} | {} | {} ",
                    app.collection.name, dirty, server, app.status
                )),
            ]);
            f.render_widget(Paragraph::new(line), area);
        }
    }
}

// ----- popups -----

fn centered(area: Rect, pct_x: u16, pct_y: u16) -> Rect {
    let [_, v, _] = Layout::vertical([
        Constraint::Percentage((100 - pct_y) / 2),
        Constraint::Percentage(pct_y),
        Constraint::Percentage((100 - pct_y) / 2),
    ])
    .areas(area);
    let [_, h, _] = Layout::horizontal([
        Constraint::Percentage((100 - pct_x) / 2),
        Constraint::Percentage(pct_x),
        Constraint::Percentage((100 - pct_x) / 2),
    ])
    .areas(v);
    h
}

fn draw_help(f: &mut Frame, app: &mut App, area: Rect) {
    let popup = centered(area, 64, 80);
    f.render_widget(Clear, popup);
    let mut lines = vec![
        Line::styled("Global", Style::default().add_modifier(Modifier::BOLD)),
        Line::raw("  1/2/3, Tab   focus sidebar / editor / response"),
        Line::raw("  z            maximize the focused pane (z again to restore)"),
        Line::raw("  ] or L       next tab (Params/Headers/Body/Docs/Variables)"),
        Line::raw("  [ or H       previous editor tab"),
        Line::raw("  /            search / filter requests"),
        Line::raw("  E            servers / base URLs"),
        Line::raw("  A            auth config (bearer / API key / OAuth2)"),
        Line::raw("  :            command line (:w save, :q quit, :q! force, :wq)"),
        Line::raw("  q            quit (warns when unsaved)"),
        Line::raw(""),
        Line::styled("Sidebar", Style::default().add_modifier(Modifier::BOLD)),
        Line::raw("  j/k, g/G     navigate"),
        Line::raw("  Enter/h/l    open request · collapse/expand group"),
        Line::raw("  n/r/d/y      new / rename / delete / duplicate request"),
        Line::raw("  /            filter (Enter keeps it, Esc clears)"),
        Line::raw("  t            cycle labels: name → summary → path"),
        Line::raw(""),
        Line::styled(
            "Editor (tables)",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::raw("  Enter        send request"),
        Line::raw("  i            edit value of selected row"),
        Line::raw("  a            add row (key then value)"),
        Line::raw("  space        enable/disable row"),
        Line::raw("  d            delete row · m cycle method · r rename"),
        Line::raw("  p            edit URL / path (paste a full URL to set"),
        Line::raw("               the server; ?query fills the Params tab;"),
        Line::raw("               a leading verb sets the method, e.g."),
        Line::raw("               `post /pets`)"),
        Line::raw(""),
        Line::styled("Body tab", Style::default().add_modifier(Modifier::BOLD)),
        Line::raw("  j/k, d/u     scroll · g/G top/bottom"),
        Line::raw("  e            edit in $EDITOR"),
        Line::raw(""),
        Line::styled("Docs tab", Style::default().add_modifier(Modifier::BOLD)),
        Line::raw("  types, enums and descriptions from the spec (* = required)"),
        Line::raw("  j/k, d/u     scroll · g/G top/bottom"),
        Line::raw(""),
        Line::styled("Response", Style::default().add_modifier(Modifier::BOLD)),
        Line::raw("  j/k, d/u     scroll · g/G top/bottom"),
        Line::raw("  e            open in $EDITOR (view only)"),
        Line::raw(""),
        Line::styled("Variables", Style::default().add_modifier(Modifier::BOLD)),
        Line::raw("  {{name}} in paths, params, headers and bodies —"),
        Line::raw("  substituted at send time. Dynamic ones, computed"),
        Line::raw("  per send ({{$name}} to bypass a same-named variable):"),
    ];
    lines.extend(DYNAMIC_VARS.iter().map(|(name, help)| {
        Line::from(vec![
            Span::styled(format!("  {name:<15}"), Style::default().fg(Color::Magenta)),
            Span::raw(*help),
        ])
    }));
    lines.extend([
        Line::raw(""),
        Line::styled("Commands", Style::default().add_modifier(Modifier::BOLD)),
        Line::raw("  :new <name>                      create a collection"),
        Line::raw("  :open <name>                     switch collection"),
        Line::raw("  :label name|summary|path         sidebar label source"),
        Line::raw("  :groups collapsed|expanded       group state on open"),
        Line::raw("  :rename-all summary|operation|path|method-path"),
    ]);

    // The list outgrows short terminals, so the popup scrolls with j/k.
    let viewport = popup.height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(viewport);
    app.help_scroll = app.help_scroll.min(max_scroll);
    let more = if app.help_scroll < max_scroll {
        " Help — j/k scroll · Esc to close "
    } else {
        " Help — Esc to close "
    };
    let block = Block::default()
        .title(more)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .scroll((app.help_scroll as u16, 0)),
        popup,
    );
}

fn draw_env(f: &mut Frame, app: &App, area: Rect) {
    let popup = centered(area, 60, 50);
    f.render_widget(Clear, popup);
    let items: Vec<ListItem> = app
        .collection
        .servers
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let marker = if i == app.collection.active_server {
                "● "
            } else {
                "  "
            };
            ListItem::new(format!("{marker}{s}"))
        })
        .collect();
    let block = Block::default()
        .title(" Servers — Enter: use · a: add · d: delete · Esc: close ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol(">");
    let mut state = ListState::default().with_selected(Some(app.env_sel));
    f.render_stateful_widget(list, popup, &mut state);
}

fn draw_auth(f: &mut Frame, app: &App, area: Rect) {
    use crate::app::AuthField;
    use crate::model::{AuthKind, AuthStyle};

    let popup = centered(area, 70, 45);
    f.render_widget(Clear, popup);
    let block = Block::default()
        .title(" Auth — j/k: field · i/Enter: edit · space: toggle · Esc: save & close ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let fields = app.auth_fields();
    let mut lines = Vec::new();
    for (i, field) in fields.iter().enumerate() {
        let value = match field {
            AuthField::Kind => toggle_row(app.auth_form.kind, AuthKind::ALL, AuthKind::title),
            AuthField::Style => toggle_row(
                app.auth_form.auth_style,
                [AuthStyle::Basic, AuthStyle::Post],
                |s| match s {
                    AuthStyle::Basic => "basic",
                    AuthStyle::Post => "post",
                },
            ),
            f if f.is_secret() && !app.auth_field_value(i).is_empty() => "••••••••".to_string(),
            _ => app.auth_field_value(i),
        };
        let style = if i == app.auth_field {
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let label = app.auth_field_label(*field);
        lines.push(
            Line::from(vec![
                Span::styled(format!(" {label:<28}"), Style::default().fg(Color::Gray)),
                Span::raw(value),
            ])
            .style(style),
        );
    }

    // A hint that secret fields understand `$(…)` command substitution.
    if fields.iter().any(|f| f.is_secret()) {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            " secret fields accept $(cmd), e.g. $(op read \"op://vault/item/field\")",
            Style::default().fg(Color::DarkGray),
        ));
    }

    f.render_widget(Paragraph::new(lines), inner);
}

/// Render a toggle field as its options with the active one bracketed, e.g.
/// `[bearer]  apikey  oauth2`.
fn toggle_row<T: PartialEq + Copy, const N: usize>(
    current: T,
    all: [T; N],
    label: impl Fn(T) -> &'static str,
) -> String {
    all.iter()
        .map(|opt| {
            let name = label(*opt);
            if *opt == current {
                format!("[{name}]")
            } else {
                format!(" {name} ")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
