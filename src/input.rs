//! Vim-style key handling.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{
    App, CellCol, EditTarget, EditorTab, ExternalEdit, Focus, Mode, Popup, SidebarRow,
};

pub fn handle_key(app: &mut App, key: KeyEvent) {
    if app.popup != Popup::None {
        handle_popup(app, key);
        return;
    }
    match app.mode {
        Mode::Command => handle_command(app, key),
        Mode::Insert => handle_insert(app, key),
        Mode::Search => handle_search(app, key),
        Mode::Normal => handle_normal(app, key),
    }
}

// ----- Normal mode -----

fn handle_normal(app: &mut App, key: KeyEvent) {
    match (key.modifiers, key.code) {
        (_, KeyCode::Char(':')) => {
            app.command.clear();
            app.mode = Mode::Command;
        }
        (_, KeyCode::Char('?')) => {
            app.help_scroll = 0;
            app.popup = Popup::Help;
        }
        (_, KeyCode::Char('q')) => app.try_quit(),
        (_, KeyCode::Tab) => {
            app.focus = match app.focus {
                Focus::Sidebar => Focus::Editor,
                Focus::Editor => Focus::Response,
                Focus::Response => Focus::Sidebar,
            };
        }
        (_, KeyCode::BackTab) => {
            app.focus = match app.focus {
                Focus::Sidebar => Focus::Response,
                Focus::Editor => Focus::Sidebar,
                Focus::Response => Focus::Editor,
            };
        }
        (_, KeyCode::Char('z')) => {
            app.zoom = !app.zoom;
            app.status = if app.zoom {
                "Pane maximized — z to restore".into()
            } else {
                "Panes restored".into()
            };
        }
        (_, KeyCode::Char('1')) => app.focus = Focus::Sidebar,
        (_, KeyCode::Char('2')) => app.focus = Focus::Editor,
        (_, KeyCode::Char('3')) => app.focus = Focus::Response,

        // Editor tab cycling. `]`/`[` are the primary bindings; `L`/`H` are
        // aliases for keyboards where the brackets are awkward to reach.
        (_, KeyCode::Char(']') | KeyCode::Char('L')) => {
            app.tab = app.tab.next();
            app.table_row = 0;
        }
        (_, KeyCode::Char('[') | KeyCode::Char('H')) => {
            app.tab = app.tab.prev();
            app.table_row = 0;
        }
        (m, KeyCode::Right) if m.contains(KeyModifiers::SHIFT) => {
            app.tab = app.tab.next();
            app.table_row = 0;
        }
        (m, KeyCode::Left) if m.contains(KeyModifiers::SHIFT) => {
            app.tab = app.tab.prev();
            app.table_row = 0;
        }

        (_, KeyCode::Char('/')) => app.start_search(),

        (_, KeyCode::Char('E')) => {
            app.env_sel = app.collection.active_server;
            app.popup = Popup::Env;
        }
        (_, KeyCode::Char('A')) => app.open_auth_popup(),

        _ => match app.focus {
            Focus::Sidebar => normal_sidebar(app, key),
            Focus::Editor => normal_editor(app, key),
            Focus::Response => normal_response(app, key),
        },
    }
}

fn normal_sidebar(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down if app.sidebar_sel + 1 < app.sidebar_rows.len() => {
            app.sidebar_sel += 1;
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.sidebar_sel = app.sidebar_sel.saturating_sub(1);
        }
        KeyCode::Char('g') => app.sidebar_sel = 0,
        KeyCode::Char('G') => {
            app.sidebar_sel = app.sidebar_rows.len().saturating_sub(1);
        }
        KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('l') | KeyCode::Char('h') => {
            app.activate_sidebar()
        }
        KeyCode::Esc => app.clear_filter(),
        KeyCode::Char('t') => app.cycle_label_mode(),
        KeyCode::Char('n') => app.start_edit(EditTarget::NewRequest),
        KeyCode::Char('r') => {
            if let Some(SidebarRow::Request(idx)) = app.sidebar_rows.get(app.sidebar_sel).cloned() {
                // Select the highlighted request but keep focus in the sidebar.
                app.select_request(idx);
                app.focus = Focus::Sidebar;
                app.start_edit(EditTarget::Rename);
            }
        }
        KeyCode::Char('d') => {
            if let Some(SidebarRow::Request(idx)) = app.sidebar_rows.get(app.sidebar_sel).cloned() {
                app.collection.requests.remove(idx);
                app.dirty = true;
                if app.selected == Some(idx) {
                    app.selected = None;
                    app.set_body_text("");
                }
                app.selected = app.selected.map(|s| if s > idx { s - 1 } else { s });
                app.rebuild_sidebar();
                app.status = "Request deleted".into();
            }
        }
        // `y` for yank, rather than something next to destructive `d`.
        KeyCode::Char('y') => {
            if let Some(SidebarRow::Request(idx)) = app.sidebar_rows.get(app.sidebar_sel).cloned() {
                app.duplicate_request(idx);
                // `duplicate_request` opens the clone, which moves focus to the
                // editor; stay here so repeated `y` works (same as `r`).
                app.focus = Focus::Sidebar;
            }
        }
        _ => {}
    }
}

fn normal_editor(app: &mut App, key: KeyEvent) {
    if app.tab == EditorTab::Body && body_scroll(app, key) {
        return;
    }
    if app.tab == EditorTab::Docs && docs_scroll(app, key) {
        return;
    }
    match key.code {
        KeyCode::Enter => app.send_selected(),
        KeyCode::Char('j') | KeyCode::Down => {
            if let Some(t) = app.tab.table() {
                let len = app.table_len(t);
                if len > 0 && app.table_row + 1 < len {
                    app.table_row += 1;
                }
            }
        }
        KeyCode::Char('k') | KeyCode::Up if app.tab.table().is_some() => {
            app.table_row = app.table_row.saturating_sub(1);
        }
        KeyCode::Char('g') => app.table_row = 0,
        KeyCode::Char('G') => {
            if let Some(t) = app.tab.table() {
                app.table_row = app.table_len(t).saturating_sub(1);
            }
        }
        KeyCode::Char(' ') => {
            if let Some(t) = app.tab.table() {
                app.toggle_row(t, app.table_row);
            }
        }
        // Body has no in-app editor — use `e` for `$EDITOR`. Tables edit inline.
        KeyCode::Char('i') | KeyCode::Char('a') => {
            let is_add = key.code == KeyCode::Char('a');
            if let Some(t) = app.tab.table() {
                if is_add {
                    app.add_row(t);
                } else if app.table_row < app.table_len(t) {
                    app.chain_to_value = false;
                    app.start_edit(EditTarget::Cell {
                        table: t,
                        row: app.table_row,
                        col: CellCol::Value,
                    });
                }
            }
        }
        KeyCode::Char('d') => {
            if let Some(t) = app.tab.table() {
                app.delete_row(t, app.table_row);
            }
        }
        KeyCode::Char('e') if app.tab == EditorTab::Body && app.selected.is_some() => {
            app.pending_external = Some(ExternalEdit::Body);
        }
        KeyCode::Char('m') => cycle_method(app),
        KeyCode::Char('r') if app.selected.is_some() => {
            app.start_edit(EditTarget::Rename);
        }
        // `p` for path/paste. Not `u` — that is page-up on every scrollable pane.
        KeyCode::Char('p') if app.selected.is_some() => {
            app.start_edit(EditTarget::Url);
        }
        _ => {}
    }
}

/// Body-tab movement. The read-only body view is a plain scroll offset,
/// clamped against the rendered height in `ui::draw_body`. Returns whether
/// the key was consumed.
fn body_scroll(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => app.body_scroll += 1,
        KeyCode::Char('k') | KeyCode::Up => app.body_scroll = app.body_scroll.saturating_sub(1),
        KeyCode::Char('g') => app.body_scroll = 0,
        KeyCode::Char('G') => app.body_scroll = usize::MAX / 2,
        KeyCode::Char('d') => app.body_scroll += 15,
        KeyCode::Char('u') => app.body_scroll = app.body_scroll.saturating_sub(15),
        _ => return false,
    }
    true
}

/// Docs-tab scrolling. The tab is read-only, so `d`/`u` page here instead of
/// deleting rows. Clamped against the rendered height in `ui::draw_docs`.
fn docs_scroll(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => app.docs_scroll += 1,
        KeyCode::Char('k') | KeyCode::Up => app.docs_scroll = app.docs_scroll.saturating_sub(1),
        KeyCode::Char('d') => app.docs_scroll += 15,
        KeyCode::Char('u') => app.docs_scroll = app.docs_scroll.saturating_sub(15),
        KeyCode::Char('g') => app.docs_scroll = 0,
        KeyCode::Char('G') => app.docs_scroll = usize::MAX / 2,
        _ => return false,
    }
    true
}

fn cycle_method(app: &mut App) {
    let Some(i) = app.selected else { return };
    use crate::model::Method::*;
    let req = &mut app.collection.requests[i];
    req.method = match req.method {
        Get => Post,
        Post => Put,
        Put => Patch,
        Patch => Delete,
        Delete => Head,
        Head => Options,
        Options => Get,
    };
    app.dirty = true;
}

fn normal_response(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => app.response_scroll += 1,
        KeyCode::Char('k') | KeyCode::Up => {
            app.response_scroll = app.response_scroll.saturating_sub(1)
        }
        KeyCode::Char('g') => app.response_scroll = 0,
        KeyCode::Char('G') => app.response_scroll = usize::MAX / 2, // clamped at render
        KeyCode::Char('d') => app.response_scroll += 15,
        KeyCode::Char('u') => app.response_scroll = app.response_scroll.saturating_sub(15),
        KeyCode::Char('e') if app.response.is_some() => {
            app.pending_external = Some(ExternalEdit::Response);
        }
        _ => {}
    }
}

// ----- Insert mode -----

fn handle_insert(app: &mut App, key: KeyEvent) {
    if app.editing.is_some() {
        match key.code {
            KeyCode::Enter => app.commit_edit(),
            KeyCode::Esc => app.cancel_edit(),
            KeyCode::Char(c) => app.input.insert(c),
            KeyCode::Backspace => app.input.backspace(),
            KeyCode::Delete => app.input.delete(),
            KeyCode::Left => app.input.left(),
            KeyCode::Right => app.input.right(),
            KeyCode::Home => app.input.home(),
            KeyCode::End => app.input.end(),
            _ => {}
        }
        return;
    }
    // No in-app body editor: body is edited via `$EDITOR` (`e`). Any stray
    // Insert-mode key just returns to Normal.
    if key.code == KeyCode::Esc {
        app.mode = Mode::Normal;
    }
}

// ----- Search mode (sidebar filter) -----

/// The filter is applied on every keystroke; `Enter` keeps it, `Esc` drops it.
fn handle_search(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Enter => {
            app.finish_search();
            return;
        }
        KeyCode::Esc => {
            app.search.set("");
            app.apply_search();
            app.mode = Mode::Normal;
            app.status = "Filter cleared".into();
            return;
        }
        KeyCode::Char(c) => app.search.insert(c),
        KeyCode::Backspace => app.search.backspace(),
        KeyCode::Delete => app.search.delete(),
        KeyCode::Left => app.search.left(),
        KeyCode::Right => app.search.right(),
        KeyCode::Home => app.search.home(),
        KeyCode::End => app.search.end(),
        KeyCode::Down => {
            // Step through matches without leaving the prompt.
            if app.sidebar_sel + 1 < app.sidebar_rows.len() {
                app.sidebar_sel += 1;
            }
            return;
        }
        KeyCode::Up => {
            app.sidebar_sel = app.sidebar_sel.saturating_sub(1);
            return;
        }
        _ => return,
    }
    app.apply_search();
}

// ----- Command mode -----

fn handle_command(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Enter => app.exec_command(),
        KeyCode::Esc => {
            app.command.clear();
            app.mode = Mode::Normal;
        }
        KeyCode::Char(c) => app.command.push(c),
        KeyCode::Backspace => {
            app.command.pop();
        }
        _ => {}
    }
}

// ----- Popups -----

fn handle_popup(app: &mut App, key: KeyEvent) {
    // While a popup field is being edited, input goes to the line editor.
    if app.editing.is_some() {
        match key.code {
            KeyCode::Enter => app.commit_edit(),
            KeyCode::Esc => app.cancel_edit(),
            KeyCode::Char(c) => app.input.insert(c),
            KeyCode::Backspace => app.input.backspace(),
            KeyCode::Delete => app.input.delete(),
            KeyCode::Left => app.input.left(),
            KeyCode::Right => app.input.right(),
            _ => {}
        }
        return;
    }

    match app.popup {
        Popup::Help => match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => app.popup = Popup::None,
            // Clamped against the rendered height in `ui::draw_help`.
            KeyCode::Char('j') | KeyCode::Down => app.help_scroll += 1,
            KeyCode::Char('k') | KeyCode::Up => app.help_scroll = app.help_scroll.saturating_sub(1),
            KeyCode::Char('d') => app.help_scroll += 10,
            KeyCode::Char('u') => app.help_scroll = app.help_scroll.saturating_sub(10),
            KeyCode::Char('g') => app.help_scroll = 0,
            KeyCode::Char('G') => app.help_scroll = usize::MAX / 2,
            _ => {}
        },
        Popup::Env => match key.code {
            KeyCode::Esc | KeyCode::Char('q') => app.popup = Popup::None,
            KeyCode::Char('j') | KeyCode::Down
                if app.env_sel + 1 < app.collection.servers.len() =>
            {
                app.env_sel += 1;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                app.env_sel = app.env_sel.saturating_sub(1);
            }
            KeyCode::Enter => {
                if app.env_sel < app.collection.servers.len() {
                    app.collection.active_server = app.env_sel;
                    app.dirty = true;
                    app.status = format!("Base URL: {}", app.collection.servers[app.env_sel]);
                }
                app.popup = Popup::None;
            }
            KeyCode::Char('a') => app.start_edit(EditTarget::EnvNew),
            KeyCode::Char('d') if app.env_sel < app.collection.servers.len() => {
                app.collection.servers.remove(app.env_sel);
                if app.collection.active_server >= app.collection.servers.len() {
                    app.collection.active_server = app.collection.servers.len().saturating_sub(1);
                }
                app.env_sel = app.env_sel.saturating_sub(1);
                app.dirty = true;
            }
            _ => {}
        },
        Popup::Auth => {
            let count = app.auth_fields().len();
            match key.code {
                KeyCode::Esc => {
                    app.apply_auth_form();
                    app.popup = Popup::None;
                    app.status = "Auth config saved".into();
                }
                KeyCode::Char('j') | KeyCode::Down | KeyCode::Tab => {
                    app.auth_field = (app.auth_field + 1) % count;
                }
                KeyCode::Char('k') | KeyCode::Up | KeyCode::BackTab => {
                    app.auth_field = (app.auth_field + count - 1) % count;
                }
                KeyCode::Enter | KeyCode::Char('i') => {
                    if app.auth_field_at(app.auth_field).is_toggle() {
                        app.toggle_auth_field(app.auth_field);
                    } else {
                        app.start_edit(EditTarget::AuthField(app.auth_field));
                    }
                }
                KeyCode::Char(' ') if app.auth_field_at(app.auth_field).is_toggle() => {
                    app.toggle_auth_field(app.auth_field);
                }
                _ => {}
            }
        }
        Popup::None => {}
    }
}

// Keep KeyModifiers referenced for future Ctrl bindings.
#[allow(dead_code)]
fn _has_ctrl(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
}
