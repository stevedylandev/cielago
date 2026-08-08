//! Application state and the TUI run loop.

use std::collections::HashSet;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tui_textarea::TextArea;
use uuid::Uuid;

use crate::http::{HttpResponse, OAuthToken, SendOutcome, send_with_auth, split_url_input};
use crate::model::{
    AuthKind, Collection, KeyValueRow, LabelMode, OAuthConfig, SavedRequest, variables_map,
};
use crate::store::{self, AppConfig};
use crate::{input, ui};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    Command,
    /// Incremental sidebar filter, opened with `/`.
    Search,
}

/// Which pane has the keyboard: the `1` / `2` / `3` panes. Persisted as part
/// of a collection's saved view, hence the serde derives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Focus {
    Sidebar,
    Editor,
    Response,
}

/// Persisted with the saved view alongside [`Focus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EditorTab {
    Params,
    Headers,
    Body,
    /// Read-only view of the spec's types, enums and descriptions.
    Docs,
    Variables,
}

impl EditorTab {
    pub const ALL: [EditorTab; 5] = [
        EditorTab::Params,
        EditorTab::Headers,
        EditorTab::Body,
        EditorTab::Docs,
        EditorTab::Variables,
    ];

    pub fn title(self) -> &'static str {
        match self {
            EditorTab::Params => "Params",
            EditorTab::Headers => "Headers",
            EditorTab::Body => "Body",
            EditorTab::Docs => "Docs",
            EditorTab::Variables => "Variables",
        }
    }

    pub fn index(self) -> usize {
        EditorTab::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    pub fn next(self) -> Self {
        EditorTab::ALL[(self.index() + 1) % EditorTab::ALL.len()]
    }

    pub fn prev(self) -> Self {
        EditorTab::ALL[(self.index() + EditorTab::ALL.len() - 1) % EditorTab::ALL.len()]
    }

    /// Tables map onto editable key/value rows; Body uses the textarea and
    /// Docs is rendered text.
    pub fn table(self) -> Option<TableId> {
        match self {
            EditorTab::Params => Some(TableId::Params),
            EditorTab::Headers => Some(TableId::Headers),
            EditorTab::Variables => Some(TableId::Vars),
            EditorTab::Body | EditorTab::Docs => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableId {
    /// Path params first, then query params (Postman-style Params tab).
    Params,
    Headers,
    Vars,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Popup {
    None,
    Help,
    Env,
    Auth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellCol {
    Key,
    Value,
}

/// What the single-line input currently edits (Insert mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditTarget {
    Cell {
        table: TableId,
        row: usize,
        col: CellCol,
    },
    Rename,
    NewRequest,
    /// The selected request's URL / path. Pasting an absolute URL here also
    /// sets the collection's server — see [`App::apply_url_input`].
    Url,
    EnvNew,
    /// Index into [`App::auth_fields`] for the current auth kind. An index (not
    /// the [`AuthField`] itself) so the enum stays `Copy`-cheap and the cursor
    /// and edit target share one notion of "which row".
    AuthField(usize),
}

/// One editable row in the auth popup. Which rows show depends on the selected
/// [`AuthKind`]; [`App::auth_fields`] builds the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthField {
    /// The scheme selector (a toggle, not a text field).
    Kind,
    /// Bearer token, or API-key value (secret).
    Token,
    /// API-key header name.
    Header,
    TokenUrl,
    ClientId,
    ClientSecret,
    Scopes,
    /// OAuth client-auth placement (a toggle, not a text field).
    Style,
}

impl AuthField {
    /// Rows that carry a secret and should render masked.
    pub fn is_secret(self) -> bool {
        matches!(self, AuthField::Token | AuthField::ClientSecret)
    }

    /// Rows edited by toggling rather than typing.
    pub fn is_toggle(self) -> bool {
        matches!(self, AuthField::Kind | AuthField::Style)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarRow {
    Group(String),
    Request(usize),
}

/// What a queued `$EDITOR` session opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalEdit {
    /// The request body; the file is read back on exit.
    Body,
    /// The response body, for paging/searching in a real editor. Saves are
    /// discarded — a response is a record of what the server returned.
    Response,
}

/// Sidebar group a request belongs to: its first spec tag, or `default`.
fn group_tag(req: &SavedRequest) -> String {
    req.tags
        .first()
        .cloned()
        .unwrap_or_else(|| "default".into())
}

/// `"list pets"` → `"list pets copy"`, then `"list pets copy 2"`, … Nothing in
/// the app keys on `name`, but three identically-labelled sidebar rows are
/// unusable. A ` copy`/` copy N` suffix on the source is stripped first, so
/// duplicating a duplicate gives `x copy 2` rather than `x copy copy`.
fn unique_request_name(requests: &[SavedRequest], base: &str) -> String {
    let stem = copy_stem(base);
    let taken = |name: &str| requests.iter().any(|r| r.name == name);
    let first = format!("{stem} copy");
    if !taken(&first) {
        return first;
    }
    (2..)
        .map(|n| format!("{stem} copy {n}"))
        .find(|candidate| !taken(candidate))
        .unwrap_or(first)
}

/// Strip a trailing ` copy` or ` copy <n>` from a request name.
fn copy_stem(name: &str) -> &str {
    if let Some(head) = name.strip_suffix(" copy") {
        return head;
    }
    let without_digits = name.trim_end_matches(|c: char| c.is_ascii_digit());
    if without_digits.len() < name.len()
        && let Some(head) = without_digits.strip_suffix(" copy ")
    {
        return head;
    }
    name
}

/// Minimal single-line editor with a cursor.
#[derive(Debug, Default, Clone)]
pub struct LineEdit {
    pub buf: String,
    pub cursor: usize,
}

impl LineEdit {
    pub fn set(&mut self, s: &str) {
        self.buf = s.to_string();
        self.cursor = self.buf.len();
    }

    pub fn insert(&mut self, c: char) {
        self.buf.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            let prev = self.cursor - self.buf[..self.cursor].chars().last().unwrap().len_utf8();
            self.buf.replace_range(prev..self.cursor, "");
            self.cursor = prev;
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.buf.len() {
            let next = self.cursor + self.buf[self.cursor..].chars().next().unwrap().len_utf8();
            self.buf.replace_range(self.cursor..next, "");
        }
    }

    pub fn left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= self.buf[..self.cursor].chars().last().unwrap().len_utf8();
        }
    }

    pub fn right(&mut self) {
        if self.cursor < self.buf.len() {
            self.cursor += self.buf[self.cursor..].chars().next().unwrap().len_utf8();
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.buf.len();
    }
}

pub struct App {
    pub collection: Collection,
    pub path: PathBuf,
    pub config: AppConfig,
    pub client: reqwest::Client,
    pub dirty: bool,
    pub should_quit: bool,

    pub mode: Mode,
    pub focus: Focus,
    pub tab: EditorTab,
    pub popup: Popup,
    /// `z`: the focused pane fills the frame and the others are not drawn.
    /// Purely a view flag — focus still moves normally underneath, so
    /// Tab/1/2/3 swap which pane is the zoomed one.
    pub zoom: bool,

    pub collapsed: HashSet<String>,
    pub sidebar_rows: Vec<SidebarRow>,
    pub sidebar_sel: usize,
    /// Active sidebar filter; empty means "show everything".
    pub filter: String,
    /// The `/` prompt buffer while [`Mode::Search`] is active.
    pub search: LineEdit,

    /// Index into `collection.requests` currently loaded in the editor.
    pub selected: Option<usize>,
    pub table_row: usize,

    pub editing: Option<EditTarget>,
    pub input: LineEdit,
    /// After committing a new row's key, continue to its value cell.
    pub chain_to_value: bool,
    pub textarea: TextArea<'static>,

    /// Scroll offset of the Docs tab, reset when another request is opened.
    pub docs_scroll: usize,

    pub response: Option<HttpResponse>,
    pub response_scroll: usize,

    pub sending: bool,
    pub tx: mpsc::UnboundedSender<SendOutcome>,
    pub rx: mpsc::UnboundedReceiver<SendOutcome>,
    pub token: Option<OAuthToken>,

    pub status: String,
    pub command: String,
    pub pending_external: Option<ExternalEdit>,
    /// Scroll offset of the help popup, which is taller than short terminals.
    pub help_scroll: usize,

    pub env_sel: usize,
    pub auth_form: OAuthConfig,
    pub auth_field: usize,
}

impl App {
    pub fn new(collection: Collection, path: PathBuf, config: AppConfig) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        let mut app = Self {
            collection,
            path,
            config,
            client,
            dirty: false,
            should_quit: false,
            mode: Mode::Normal,
            focus: Focus::Sidebar,
            tab: EditorTab::Params,
            popup: Popup::None,
            zoom: false,
            collapsed: HashSet::new(),
            sidebar_rows: Vec::new(),
            sidebar_sel: 0,
            filter: String::new(),
            search: LineEdit::default(),
            selected: None,
            table_row: 0,
            editing: None,
            input: LineEdit::default(),
            chain_to_value: false,
            textarea: TextArea::default(),
            docs_scroll: 0,
            response: None,
            response_scroll: 0,
            sending: false,
            tx,
            rx,
            token: None,
            status: "Press ? for help".to_string(),
            command: String::new(),
            pending_external: None,
            help_scroll: 0,
            env_sel: 0,
            auth_form: OAuthConfig::default(),
            auth_field: 0,
        };
        if app.collection.groups_collapsed {
            app.collapsed = app.group_tags();
        }
        app.rebuild_sidebar();
        if !app.collection.requests.is_empty() {
            // Restore the request that was open when the collection was last
            // saved; failing that, load the first one so the editor isn't
            // blank.
            match app.saved_view_index() {
                Some(idx) => app.restore_saved_view(idx),
                None => app.select_request(0),
            }
        }
        app.restore_saved_panes();
        if app.collection.requests.is_empty() {
            app.status = "Empty collection — press n to add a request".into();
        }
        app
    }

    // ----- saved view -----

    /// Index of `collection.last_request`, if that request still exists. Ids
    /// are matched rather than positions so a re-import that reorders or drops
    /// operations can't restore the wrong request.
    fn saved_view_index(&self) -> Option<usize> {
        let id = self.collection.last_request?;
        self.collection.requests.iter().position(|r| r.id == id)
    }

    /// Restore the focused pane and editor tab from the saved view. Without
    /// one, focus starts on the sidebar (`select_request` leaves it on the
    /// editor): opening a collection, the first move is picking which request
    /// to work on.
    fn restore_saved_panes(&mut self) {
        if let Some(tab) = self.collection.last_tab {
            self.tab = tab;
        }
        self.focus = match self.collection.last_focus {
            // Responses aren't persisted, so a saved Response pane is empty on
            // open — land on the editor instead of a pane with nothing in it.
            Some(Focus::Response) if self.response.is_none() => Focus::Editor,
            Some(focus) => focus,
            None => Focus::Sidebar,
        };
    }

    /// Open `idx` and park the sidebar cursor on it. Expands the containing
    /// group if needed: with `groups_collapsed` set, the restored request would
    /// otherwise be scrolled to but invisible.
    fn restore_saved_view(&mut self, idx: usize) {
        let tag = group_tag(&self.collection.requests[idx]);
        if self.collapsed.remove(&tag) {
            self.rebuild_sidebar();
        }
        if let Some(pos) = self
            .sidebar_rows
            .iter()
            .position(|r| *r == SidebarRow::Request(idx))
        {
            self.sidebar_sel = pos;
        }
        self.select_request(idx);
    }

    // ----- sidebar -----

    /// Every group tag in the collection, filter ignored.
    fn group_tags(&self) -> HashSet<String> {
        self.collection.requests.iter().map(group_tag).collect()
    }

    pub fn rebuild_sidebar(&mut self) {
        let filtering = !self.filter.is_empty();
        let mut rows = Vec::new();
        let mut groups: Vec<String> = Vec::new();
        for (i, req) in self.collection.requests.iter().enumerate() {
            if filtering && !req.matches(&self.filter) {
                continue;
            }
            let tag = group_tag(req);
            if !groups.contains(&tag) {
                groups.push(tag.clone());
                rows.push(SidebarRow::Group(tag.clone()));
            }
            // While filtering, matches are always shown — a collapsed group
            // would otherwise hide the thing being searched for.
            if filtering || !self.collapsed.contains(&tag) {
                rows.push(SidebarRow::Request(i));
            }
        }
        self.sidebar_rows = rows;
        if self.sidebar_sel >= self.sidebar_rows.len() {
            self.sidebar_sel = self.sidebar_rows.len().saturating_sub(1);
        }
    }

    // ----- sidebar search -----

    pub fn start_search(&mut self) {
        self.search.set(&self.filter);
        self.focus = Focus::Sidebar;
        self.mode = Mode::Search;
    }

    /// Re-apply the live `/` buffer as the filter and land the cursor on the
    /// first matching request.
    pub fn apply_search(&mut self) {
        self.filter = self.search.buf.clone();
        self.rebuild_sidebar();
        if let Some(pos) = self
            .sidebar_rows
            .iter()
            .position(|r| matches!(r, SidebarRow::Request(_)))
        {
            self.sidebar_sel = pos;
        }
    }

    pub fn finish_search(&mut self) {
        self.mode = Mode::Normal;
        self.status = if self.filter.is_empty() {
            "Filter cleared".into()
        } else {
            let n = self
                .sidebar_rows
                .iter()
                .filter(|r| matches!(r, SidebarRow::Request(_)))
                .count();
            format!("Filter \"{}\" — {n} request(s) · Esc clears", self.filter)
        };
    }

    pub fn clear_filter(&mut self) {
        if self.filter.is_empty() {
            return;
        }
        self.filter.clear();
        self.search.set("");
        self.rebuild_sidebar();
        self.status = "Filter cleared".into();
    }

    // ----- request labels -----

    pub fn cycle_label_mode(&mut self) {
        self.collection.label_mode = self.collection.label_mode.next();
        self.dirty = true;
        self.status = format!("Sidebar labels: {}", self.collection.label_mode.title());
    }

    pub fn activate_sidebar(&mut self) {
        match self.sidebar_rows.get(self.sidebar_sel).cloned() {
            Some(SidebarRow::Group(tag)) => {
                if !self.collapsed.remove(&tag) {
                    self.collapsed.insert(tag);
                }
                self.rebuild_sidebar();
            }
            Some(SidebarRow::Request(idx)) => self.select_request(idx),
            None => {}
        }
    }

    // ----- request selection -----

    pub fn selected_request(&self) -> Option<&SavedRequest> {
        self.selected.map(|i| &self.collection.requests[i])
    }

    pub fn select_request(&mut self, idx: usize) {
        self.commit_body();
        self.selected = Some(idx);
        self.table_row = 0;
        self.docs_scroll = 0;
        let body = self.collection.requests[idx]
            .body
            .clone()
            .unwrap_or_default();
        self.set_textarea_text(&body);
        self.focus = Focus::Editor;
    }

    pub fn set_textarea_text(&mut self, text: &str) {
        let lines: Vec<String> = if text.is_empty() {
            vec![String::new()]
        } else {
            text.lines().map(String::from).collect()
        };
        self.textarea = TextArea::from(lines);
        self.textarea
            .set_cursor_line_style(ratatui::style::Style::default());
    }

    /// Write the textarea contents back into the selected request body.
    pub fn commit_body(&mut self) {
        let Some(idx) = self.selected else { return };
        let text = self.textarea.lines().join("\n");
        let text = text.trim_end_matches('\n').to_string();
        let req = &mut self.collection.requests[idx];
        let new = if text.trim().is_empty() {
            None
        } else {
            Some(text)
        };
        if req.body != new {
            req.body = new;
            self.dirty = true;
        }
    }

    // ----- tables (params / headers / variables) -----

    pub fn table_len(&self, table: TableId) -> usize {
        let Some(req) = self.selected_request() else {
            return if table == TableId::Vars {
                self.collection.variables.len()
            } else {
                0
            };
        };
        match table {
            TableId::Params => req.path_params.len() + req.query.len(),
            TableId::Headers => req.headers.len(),
            TableId::Vars => self.collection.variables.len(),
        }
    }

    fn row_ref(&self, table: TableId, row: usize) -> Option<&KeyValueRow> {
        match table {
            TableId::Vars => self.collection.variables.get(row),
            TableId::Headers => self.selected_request()?.headers.get(row),
            TableId::Params => {
                let req = self.selected_request()?;
                let np = req.path_params.len();
                if row < np {
                    req.path_params.get(row)
                } else {
                    req.query.get(row - np)
                }
            }
        }
    }

    fn row_mut(&mut self, table: TableId, row: usize) -> Option<&mut KeyValueRow> {
        match table {
            TableId::Vars => self.collection.variables.get_mut(row),
            TableId::Headers => {
                let i = self.selected?;
                self.collection.requests.get_mut(i)?.headers.get_mut(row)
            }
            TableId::Params => {
                let i = self.selected?;
                let req = self.collection.requests.get_mut(i)?;
                let np = req.path_params.len();
                if row < np {
                    req.path_params.get_mut(row)
                } else {
                    req.query.get_mut(row - np)
                }
            }
        }
    }

    pub fn row_value(&self, table: TableId, row: usize, col: CellCol) -> Option<String> {
        let r = self.row_ref(table, row)?;
        Some(match col {
            CellCol::Key => r.key.clone(),
            CellCol::Value => r.value.clone(),
        })
    }

    pub fn toggle_row(&mut self, table: TableId, row: usize) {
        if let Some(r) = self.row_mut(table, row) {
            r.enabled = !r.enabled;
            self.dirty = true;
        }
    }

    pub fn delete_row(&mut self, table: TableId, row: usize) {
        let removed = match table {
            TableId::Vars => {
                if row < self.collection.variables.len() {
                    self.collection.variables.remove(row);
                    true
                } else {
                    false
                }
            }
            TableId::Headers => match self.selected {
                Some(i) if row < self.collection.requests[i].headers.len() => {
                    self.collection.requests[i].headers.remove(row);
                    true
                }
                _ => false,
            },
            TableId::Params => match self.selected {
                Some(i) => {
                    let req = &mut self.collection.requests[i];
                    let np = req.path_params.len();
                    if row < np {
                        req.path_params.remove(row);
                        true
                    } else if row - np < req.query.len() {
                        req.query.remove(row - np);
                        true
                    } else {
                        false
                    }
                }
                None => false,
            },
        };
        if removed {
            self.dirty = true;
            let len = self.table_len(table);
            if self.table_row >= len {
                self.table_row = len.saturating_sub(1);
            }
        }
    }

    /// Append an empty row and start editing its key (value edit chains after).
    pub fn add_row(&mut self, table: TableId) {
        let row = match table {
            TableId::Vars => {
                self.collection
                    .variables
                    .push(KeyValueRow::new("", "", true));
                self.collection.variables.len() - 1
            }
            TableId::Headers => {
                let Some(i) = self.selected else { return };
                self.collection.requests[i]
                    .headers
                    .push(KeyValueRow::new("", "", true));
                self.collection.requests[i].headers.len() - 1
            }
            TableId::Params => {
                let Some(i) = self.selected else { return };
                // New params are query params; path params come from the path.
                self.collection.requests[i]
                    .query
                    .push(KeyValueRow::new("", "", true));
                self.collection.requests[i].path_params.len()
                    + self.collection.requests[i].query.len()
                    - 1
            }
        };
        self.dirty = true;
        self.table_row = row;
        self.start_edit(EditTarget::Cell {
            table,
            row,
            col: CellCol::Key,
        });
        self.chain_to_value = true;
    }

    // ----- editing -----

    pub fn start_edit(&mut self, target: EditTarget) {
        let initial = match target {
            EditTarget::Cell { table, row, col } => {
                self.row_value(table, row, col).unwrap_or_default()
            }
            EditTarget::Rename => self
                .selected_request()
                .map(|r| r.name.clone())
                .unwrap_or_default(),
            // Prefill the path only, not `base_url() + path`: re-serializing
            // the full URL would rebuild the query from the table and lose each
            // row's `enabled` flag. The origin is visible in the URL bar anyway.
            // A bare `/` (what `SavedRequest::blank` gives a new request) is
            // dropped, so pasting a URL into a fresh request isn't prefixed by it.
            EditTarget::Url => self
                .selected_request()
                .map(|r| r.path.clone())
                .filter(|p| p != "/")
                .unwrap_or_default(),
            EditTarget::NewRequest | EditTarget::EnvNew => String::new(),
            EditTarget::AuthField(i) => self.auth_field_value(i),
        };
        self.input.set(&initial);
        self.editing = Some(target);
        self.mode = Mode::Insert;
    }

    pub fn cancel_edit(&mut self) {
        self.editing = None;
        self.chain_to_value = false;
        self.mode = Mode::Normal;
    }

    pub fn commit_edit(&mut self) {
        let Some(target) = self.editing.take() else {
            return;
        };
        let value = self.input.buf.trim().to_string();
        self.mode = Mode::Normal;

        match target {
            EditTarget::Cell { table, row, col } => {
                if let Some(r) = self.row_mut(table, row) {
                    match col {
                        CellCol::Key => r.key = value,
                        CellCol::Value => r.value = value,
                    }
                    self.dirty = true;
                }
                if col == CellCol::Key && self.chain_to_value {
                    self.chain_to_value = false;
                    self.start_edit(EditTarget::Cell {
                        table,
                        row,
                        col: CellCol::Value,
                    });
                }
            }
            EditTarget::Rename => {
                if !value.is_empty()
                    && let Some(i) = self.selected
                {
                    self.collection.requests[i].name = value;
                    self.dirty = true;
                }
            }
            EditTarget::NewRequest => {
                if !value.is_empty() {
                    let req = SavedRequest::blank(value);
                    self.collection.requests.push(req);
                    self.dirty = true;
                    self.rebuild_sidebar();
                    let idx = self.collection.requests.len() - 1;
                    // Move sidebar selection to the new request.
                    if let Some(pos) = self
                        .sidebar_rows
                        .iter()
                        .position(|r| *r == SidebarRow::Request(idx))
                    {
                        self.sidebar_sel = pos;
                    }
                    self.select_request(idx);
                    // `blank` gives you `GET /`, which is sendable but useless;
                    // chain straight into the URL so a new request is usable in
                    // one flow.
                    self.start_edit(EditTarget::Url);
                }
            }
            EditTarget::Url => self.apply_url_input(&value),
            EditTarget::EnvNew => {
                if !value.is_empty() {
                    self.collection.servers.push(value);
                    self.collection.active_server = self.collection.servers.len() - 1;
                    self.env_sel = self.collection.active_server;
                    self.dirty = true;
                }
            }
            EditTarget::AuthField(i) => {
                self.set_auth_field(i, &value);
            }
        }
    }

    // ----- url bar -----

    /// Apply a URL-bar entry to the selected request. An absolute URL
    /// contributes its origin to `collection.servers` — added if new, made
    /// active either way — the rest becomes `req.path`, and a query string (only
    /// if the input actually had one) replaces the query rows.
    pub fn apply_url_input(&mut self, input: &str) {
        let Some(i) = self.selected else {
            self.status = "No request selected".into();
            return;
        };
        // Something that names a scheme but isn't http(s) is a typo, not a
        // relative path — say so rather than filing it under `path`. The
        // scheme-shape check matters: it keeps a stray `/api/https://…` out of
        // this branch, where the error would be more confusing than the path.
        if let Some((scheme, _)) = input.trim().split_once("://")
            && !scheme.is_empty()
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            && !matches!(scheme, "http" | "https")
        {
            self.status = format!("Only http(s) URLs are supported (got {scheme:?}://)");
            return;
        }

        let parts = split_url_input(input);
        let mut notes: Vec<String> = Vec::new();
        if let Some(origin) = parts.origin {
            // Compare trimmed: servers added with `E` may carry a trailing
            // slash, imported ones never do.
            match self
                .collection
                .servers
                .iter()
                .position(|s| s.trim_end_matches('/') == origin)
            {
                Some(idx) => {
                    if self.collection.active_server != idx {
                        self.collection.active_server = idx;
                        notes.push(format!("server → {origin}"));
                    }
                }
                None => {
                    self.collection.servers.push(origin.clone());
                    self.collection.active_server = self.collection.servers.len() - 1;
                    notes.push(format!("server + {origin}"));
                }
            }
        }

        let req = &mut self.collection.requests[i];
        req.path = parts.path;
        if let Some(query) = parts.query {
            notes.push(format!("{} query param(s)", query.len()));
            req.query = query;
        }
        req.sync_path_params();
        self.dirty = true;
        self.table_row = 0;
        let path = self.collection.requests[i].path.clone();
        self.status = if notes.is_empty() {
            format!("Path: {path}")
        } else {
            format!("Path: {path} · {}", notes.join(" · "))
        };
    }

    // ----- duplicating -----

    /// Clone the request at `idx` as a starting point: fresh id, name suffixed
    /// `copy` / `copy 2` / …, inserted directly after the original so it lands
    /// beside it in the same sidebar group. Selects the clone.
    pub fn duplicate_request(&mut self, idx: usize) {
        // Must precede the insert: `select_request` commits the textarea into
        // `requests[selected]`, and the indices shift underneath it.
        self.commit_body();
        let Some(src) = self.collection.requests.get(idx) else {
            return;
        };
        let mut clone = src.clone();
        // A fresh id is required, not cosmetic: `saved_view_index` resolves
        // `last_request` with `position(|r| r.id == id)`, so a duplicate id
        // would make reopening the collection ambiguous.
        clone.id = Uuid::new_v4();
        clone.name = unique_request_name(&self.collection.requests, &src.name);
        let at = idx + 1;
        self.collection.requests.insert(at, clone);
        if let Some(s) = self.selected.filter(|s| *s >= at) {
            self.selected = Some(s + 1);
        }
        self.dirty = true;
        self.rebuild_sidebar();
        // An active filter may hide the clone, in which case the cursor stays put.
        if let Some(pos) = self
            .sidebar_rows
            .iter()
            .position(|r| *r == SidebarRow::Request(at))
        {
            self.sidebar_sel = pos;
        }
        self.select_request(at);
        self.status = format!("Duplicated as \"{}\"", self.collection.requests[at].name);
    }

    // ----- auth popup -----

    pub fn open_auth_popup(&mut self) {
        // A brand-new config defaults to bearer — the simplest scheme, and the
        // one this popup mostly exists to make reachable. Existing configs open
        // on whatever `kind` they were saved with.
        self.auth_form = self.collection.auth.clone().unwrap_or(OAuthConfig {
            kind: AuthKind::Bearer,
            ..Default::default()
        });
        self.auth_field = 0;
        self.popup = Popup::Auth;
    }

    /// The rows shown for the form's current auth kind, in display order. Always
    /// leads with [`AuthField::Kind`] so the scheme is switchable from any state.
    pub fn auth_fields(&self) -> Vec<AuthField> {
        let mut fields = vec![AuthField::Kind];
        match self.auth_form.kind {
            AuthKind::Bearer => fields.push(AuthField::Token),
            AuthKind::ApiKey => fields.extend([AuthField::Header, AuthField::Token]),
            AuthKind::Oauth2 => fields.extend([
                AuthField::TokenUrl,
                AuthField::ClientId,
                AuthField::ClientSecret,
                AuthField::Scopes,
                AuthField::Style,
            ]),
        }
        fields
    }

    /// The [`AuthField`] under the cursor, resolving `auth_field` against the
    /// current kind's row list (clamped, so a stale index never panics).
    pub fn auth_field_at(&self, i: usize) -> AuthField {
        let fields = self.auth_fields();
        fields[i.min(fields.len() - 1)]
    }

    pub fn auth_field_label(&self, field: AuthField) -> &'static str {
        match field {
            AuthField::Kind => "Auth type",
            AuthField::Token => match self.auth_form.kind {
                AuthKind::ApiKey => "API key value",
                _ => "Bearer token",
            },
            AuthField::Header => "Header name",
            AuthField::TokenUrl => "Token URL",
            AuthField::ClientId => "Client ID",
            AuthField::ClientSecret => "Client Secret",
            AuthField::Scopes => "Scopes (space separated)",
            AuthField::Style => "Auth style",
        }
    }

    pub fn auth_field_value(&self, i: usize) -> String {
        match self.auth_field_at(i) {
            AuthField::Kind => self.auth_form.kind.title().to_string(),
            AuthField::Token => self.auth_form.token.clone(),
            AuthField::Header => self.auth_form.header.clone(),
            AuthField::TokenUrl => self.auth_form.token_url.clone(),
            AuthField::ClientId => self.auth_form.client_id.clone(),
            AuthField::ClientSecret => self.auth_form.client_secret.clone(),
            AuthField::Scopes => self.auth_form.scopes.join(" "),
            AuthField::Style => match self.auth_form.auth_style {
                crate::model::AuthStyle::Basic => "basic".into(),
                crate::model::AuthStyle::Post => "post".into(),
            },
        }
    }

    pub fn set_auth_field(&mut self, i: usize, value: &str) {
        match self.auth_field_at(i) {
            AuthField::Token => self.auth_form.token = value.to_string(),
            AuthField::Header => self.auth_form.header = value.to_string(),
            AuthField::TokenUrl => self.auth_form.token_url = value.to_string(),
            AuthField::ClientId => self.auth_form.client_id = value.to_string(),
            AuthField::ClientSecret => self.auth_form.client_secret = value.to_string(),
            AuthField::Scopes => {
                self.auth_form.scopes = value.split_whitespace().map(String::from).collect()
            }
            // Toggles carry no typed value.
            AuthField::Kind | AuthField::Style => {}
        }
    }

    /// Advance the toggle under the cursor. `Kind` cycles the scheme (which
    /// changes the row list — the cursor stays put on `Kind` at index 0), and
    /// `Style` flips the OAuth client-auth placement. No-op on text fields.
    pub fn toggle_auth_field(&mut self, i: usize) {
        match self.auth_field_at(i) {
            AuthField::Kind => self.auth_form.kind = self.auth_form.kind.next(),
            AuthField::Style => {
                self.auth_form.auth_style = match self.auth_form.auth_style {
                    crate::model::AuthStyle::Basic => crate::model::AuthStyle::Post,
                    crate::model::AuthStyle::Post => crate::model::AuthStyle::Basic,
                }
            }
            _ => {}
        }
    }

    /// Apply the auth form to the collection (called when the popup closes). A
    /// form with no meaningful field set clears auth entirely, so cycling to a
    /// scheme and leaving it blank doesn't attach an unusable config.
    pub fn apply_auth_form(&mut self) {
        let f = &self.auth_form;
        let empty = f.token.is_empty()
            && f.header.is_empty()
            && f.token_url.is_empty()
            && f.client_id.is_empty()
            && f.client_secret.is_empty()
            && f.scopes.is_empty();
        let new = if empty {
            None
        } else {
            Some(self.auth_form.clone())
        };
        if self.collection.auth != new {
            self.collection.auth = new;
            self.dirty = true;
        }
    }

    // ----- sending -----

    pub fn send_selected(&mut self) {
        self.commit_body();
        let Some(idx) = self.selected else {
            self.status = "No request selected".into();
            return;
        };
        let Some(base) = self.collection.base_url().map(String::from) else {
            self.status = "No server configured — press E to add a base URL".into();
            return;
        };
        let req = self.collection.requests[idx].clone();
        let vars = variables_map(&self.collection.variables);
        let auth = self.collection.auth.clone();
        let token = self.token.take();
        let client = self.client.clone();
        let tx = self.tx.clone();

        self.sending = true;
        self.status = format!("Sending {} {} …", req.method, req.path);
        tokio::spawn(async move {
            let outcome = send_with_auth(&client, &base, &req, &vars, auth.as_ref(), token).await;
            let _ = tx.send(outcome);
        });
    }

    pub fn handle_outcome(&mut self, outcome: SendOutcome) {
        self.sending = false;
        self.token = outcome.token;
        match outcome.result {
            Ok(resp) => {
                self.status = resp.status_line();
                self.response = Some(resp);
                self.response_scroll = 0;
            }
            Err(e) => {
                self.status = e;
            }
        }
    }

    // ----- persistence / quit -----

    /// Copy the current view (open request, pane, editor tab) onto the
    /// collection. Called from [`App::save`] rather than from the navigation
    /// handlers: marking the collection dirty every time the cursor moves
    /// would make `:q` complain about unsaved changes after a read-only browse.
    pub fn record_view(&mut self) {
        self.collection.last_request = self.selected_request().map(|r| r.id);
        self.collection.last_focus = Some(self.focus);
        self.collection.last_tab = Some(self.tab);
    }

    pub fn save(&mut self) {
        self.commit_body();
        self.record_view();
        match store::save_collection(&self.collection) {
            Ok(path) => {
                self.dirty = false;
                self.status = format!("Saved to {}", path.display());
            }
            Err(e) => self.status = format!("Save failed: {e:#}"),
        }
    }

    // ----- switching collections -----

    /// Replace the whole app state with a different collection, keeping the
    /// process and terminal alive. Everything view-related is derived from the
    /// collection by [`App::new`], so a wholesale reassign is both the smallest
    /// and the safest option: it also drops the send channel (so a response
    /// still in flight for the old collection can't land in the new one) and the
    /// cached OAuth token, which belonged to the old collection's auth config.
    ///
    /// Deliberately does not persist `AppConfig`: staying filesystem-free keeps
    /// this callable from tests (`store::config_dir` is hard-wired to the real
    /// home directory). The two callers below write it once they've committed.
    pub fn switch_collection(&mut self, collection: Collection, path: PathBuf) {
        let name = collection.name.clone();
        let mut config = std::mem::take(&mut self.config);
        config.last_collection = Some(name.clone());
        *self = App::new(collection, path, config);
        // `App::new` sets its own status; ours is the more useful one here.
        self.status = format!("Switched to \"{name}\"");
    }

    /// `:new <name>` — create an empty collection on disk and switch to it.
    fn new_collection(&mut self, name: &str, force: bool) {
        if name.is_empty() {
            self.status = "Usage: :new <collection name>".into();
            return;
        }
        if self.dirty && !force {
            self.status = "Unsaved changes — :w first, or :new! to discard".into();
            return;
        }
        let path = match store::collection_path(name) {
            Ok(p) => p,
            Err(e) => {
                self.status = format!("{e:#}");
                return;
            }
        };
        // Checks the slug path, so a name that collides after slugify is caught.
        if path.exists() {
            self.status = format!("A collection already exists at {}", path.display());
            return;
        }
        let collection = Collection::new(name);
        match store::save_collection(&collection) {
            Ok(path) => {
                self.switch_collection(collection, path);
                let _ = self.config.save();
            }
            Err(e) => self.status = format!("Could not create {name:?}: {e:#}"),
        }
    }

    /// `:open <name>` — switch to another saved collection.
    fn open_collection(&mut self, name: &str, force: bool) {
        if name.is_empty() {
            self.status = "Usage: :open <collection name>".into();
            return;
        }
        if self.dirty && !force {
            self.status = "Unsaved changes — :w first, or :open! to discard".into();
            return;
        }
        let loaded = store::resolve_collection(name)
            .and_then(|n| Ok((store::load_collection(&n)?, store::collection_path(&n)?)));
        match loaded {
            Ok((collection, path)) => {
                self.switch_collection(collection, path);
                let _ = self.config.save();
            }
            // `resolve_collection`'s error already lists what is available.
            Err(e) => self.status = format!("{e:#}").replace('\n', " "),
        }
    }

    pub fn try_quit(&mut self) {
        if self.dirty {
            self.status = "Unsaved changes — use :q! to discard, :w to save".into();
        } else {
            self.should_quit = true;
        }
    }

    pub fn exec_command(&mut self) {
        let cmd = self.command.trim().to_string();
        self.command.clear();
        self.mode = Mode::Normal;
        match cmd.as_str() {
            "w" => self.save(),
            "q" => self.try_quit(),
            "q!" => self.should_quit = true,
            "wq" => {
                self.save();
                if !self.dirty {
                    self.should_quit = true;
                }
            }
            // Argument-less forms; the command is already trimmed, so these
            // never reach the `split_once` arms below.
            "new" | "new!" => self.new_collection("", false),
            "open" | "open!" => self.open_collection("", false),
            "" => {}
            other => match other.split_once(char::is_whitespace) {
                Some(("new", arg)) => self.new_collection(arg.trim(), false),
                Some(("new!", arg)) => self.new_collection(arg.trim(), true),
                Some(("open", arg)) => self.open_collection(arg.trim(), false),
                Some(("open!", arg)) => self.open_collection(arg.trim(), true),
                Some(("label", arg)) => self.set_label_mode(arg.trim()),
                Some(("groups", arg)) => self.set_group_default(arg.trim()),
                Some(("rename-all", arg)) => self.rename_all(arg.trim()),
                _ => self.status = format!("Unknown command: {other}"),
            },
        }
    }

    fn set_label_mode(&mut self, arg: &str) {
        let mode = match arg {
            "name" => LabelMode::Name,
            "summary" => LabelMode::Summary,
            "path" => LabelMode::Path,
            other => {
                self.status = format!("Usage: :label name|summary|path (got {other:?})");
                return;
            }
        };
        self.collection.label_mode = mode;
        self.dirty = true;
        self.status = format!("Sidebar labels: {}", mode.title());
    }

    /// Set whether groups start collapsed, and apply it to the current view so
    /// the effect is visible without reopening the collection.
    fn set_group_default(&mut self, arg: &str) {
        let collapsed = match arg {
            "collapsed" => true,
            "expanded" => false,
            other => {
                self.status = format!("Usage: :groups collapsed|expanded (got {other:?})");
                return;
            }
        };
        self.collection.groups_collapsed = collapsed;
        self.collapsed = if collapsed {
            self.group_tags()
        } else {
            HashSet::new()
        };
        self.rebuild_sidebar();
        self.dirty = true;
        self.status = format!("Groups default: {arg}");
    }

    /// Rewrite every request's `name` from a spec-derived field. Unlike
    /// `:label`, this is destructive — it replaces the stored names.
    fn rename_all(&mut self, arg: &str) {
        let mut renamed = 0usize;
        for req in &mut self.collection.requests {
            let new = match arg {
                "summary" => req.summary.clone(),
                "operation" => req.operation_id.clone(),
                "path" => Some(req.path.clone()),
                "method-path" => Some(format!("{} {}", req.method, req.path)),
                other => {
                    self.status = format!(
                        "Usage: :rename-all summary|operation|path|method-path (got {other:?})"
                    );
                    return;
                }
            };
            if let Some(new) = new.filter(|s| !s.is_empty())
                && req.name != new
            {
                req.name = new;
                renamed += 1;
            }
        }
        if renamed > 0 {
            self.dirty = true;
        }
        self.status = format!("Renamed {renamed} request(s) from {arg}");
    }
}

// ----- run loop -----

pub async fn run(collection: Collection, path: PathBuf, config: AppConfig) -> Result<()> {
    let mut app = App::new(collection, path, config);

    // Restore the terminal even if the TUI panics.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut app, &mut terminal).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

async fn run_loop(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    loop {
        terminal.draw(|f| ui::draw(f, app))?;

        while let Ok(outcome) = app.rx.try_recv() {
            app.handle_outcome(outcome);
        }

        if let Some(target) = app.pending_external {
            run_external_edit(app, terminal, target)?;
        }

        if event::poll(Duration::from_millis(60))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            input::handle_key(app, key);
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

/// Open a request or response body in `$EDITOR`: suspend the TUI, edit a temp
/// file, resume. Request bodies are read back; responses are view-only.
fn run_external_edit(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    target: ExternalEdit,
) -> Result<()> {
    app.pending_external = None;

    let content = match target {
        ExternalEdit::Body => {
            app.commit_body();
            app.selected_request()
                .and_then(|r| r.body.clone())
                .unwrap_or_default()
        }
        ExternalEdit::Response => match app.response.as_ref() {
            Some(resp) => resp.body.clone(),
            None => {
                app.status = "No response to open".into();
                return Ok(());
            }
        },
    };

    let stem = match target {
        ExternalEdit::Body => "body",
        ExternalEdit::Response => "response",
    };
    let mut tmp = std::env::temp_dir();
    tmp.push(format!(
        "cielago-{stem}-{}.{}",
        std::process::id(),
        guess_extension(&content)
    ));
    std::fs::write(&tmp, &content)?;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

    let editor = app.config.editor_cmd();
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vi");
    let status = Command::new(program).args(parts).arg(&tmp).status();

    enable_raw_mode()?;
    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    terminal.clear()?;

    match (target, status) {
        (ExternalEdit::Body, Ok(s)) if s.success() => {
            let content = std::fs::read_to_string(&tmp)?;
            if let Some(i) = app.selected {
                app.collection.requests[i].body = Some(content.clone());
                app.dirty = true;
            }
            app.set_textarea_text(&content);
            app.status = "Body updated from editor".into();
        }
        (ExternalEdit::Body, Ok(s)) => {
            app.status = format!("Editor exited with {s}; body unchanged")
        }
        // Nothing is read back: the response stays exactly as received.
        (ExternalEdit::Response, Ok(_)) => app.status = "Response closed — unchanged".into(),
        (_, Err(e)) => app.status = format!("Could not launch editor: {e}"),
    }
    let _ = std::fs::remove_file(&tmp);
    Ok(())
}

/// Extension for the temp file, so the editor picks sane syntax highlighting.
fn guess_extension(content: &str) -> &'static str {
    match content.trim_start().chars().next() {
        Some('{') | Some('[') => "json",
        Some('<') => "xml",
        _ => "txt",
    }
}
