# AGENTS.md

Notes for whoever (human or agent) works on this codebase next.

## Layout

```
src/
├── main.rs            # clap CLI: import / list / open (default) / info / edit / rename / delete / path
├── lib.rs             # re-exports the modules below for the binary + tests
├── app.rs             # App state, vim mode machine, ratatui run loop
├── input.rs           # keymap: Normal / Insert / Command modes, popups
├── ui.rs               # rendering: sidebar, url bar, editor tabs, response, popups
├── highlight.rs         # JSON/XML tokenizer -> styled ratatui Lines
├── model.rs            # Collection / SavedRequest / KeyValueRow / FieldDoc / OAuthConfig+AuthKind (serde)
├── store.rs            # ~/.config/cielago persistence, AppConfig
├── openapi/
│   ├── loader.rs        # load spec from file path or http(s) URL, JSON/YAML
│   ├── resolve.rs        # local `#/...` $ref resolution (cycle-safe)
│   ├── examples.rs        # schema -> example JSON value generation
│   ├── docs.rs             # schema -> FieldDoc (types, enums) for the Docs tab
│   └── import.rs            # Spec -> Collection conversion
└── http/
    ├── client.rs           # reqwest request building + response capture
    ├── oauth.rs              # client-credentials token exchange
    ├── secret.rs              # $(cmd) secret resolution via `sh -c`
    ├── send.rs                # send_with_auth: pick scheme; oauth cache/refresh + 401 retry
    ├── url_input.rs            # pasted URL -> origin / path / query (inverse of build_url)
    └── vars.rs                 # {{name}} + dynamic ({{uuid}}, {{randomInt}}…) substitution
```

`tests/` has fixture-driven integration tests: `import_tests.rs` (spec →
collection), `http_tests.rs`/`app_send_tests.rs` (wiremock-backed HTTP +
OAuth), `input_tests.rs` (full keymap flows over an in-memory `App`),
`ui_tests.rs` (draws into a ratatui `TestBackend` and asserts on cell colours
— the only place rendering is covered).

## Design decisions worth knowing

- **The project was renamed `getman` → `stableman` → `manpost` → `cielago`.**
  `store::config_dir` moves a leftover `~/.config/{manpost,stableman,getman}`
  onto `~/.config/cielago` on first use (see `LEGACY_DIR_NAMES`), so existing
  collections survive. Drop those migrations once they've had time to run
  everywhere.
- **No remote `$ref`s.** `openapi::resolve` only follows local
  `#/components/...` JSON pointers. Specs that split across files aren't
  supported — bundle them first if you hit this.
- **Auth is one struct, three schemes.** `OAuthConfig` (aliased `AuthConfig`)
  carries every scheme's fields, discriminated by `AuthKind` (`bearer` /
  `apikey` / `oauth2`). It stays a single flat JSON object so collections
  written before bearer/api-key support — whose `auth` has no `kind` — still
  deserialize (missing `kind` defaults to `oauth2`, which is what they were).
  The popup (`A`) builds its rows from `App::auth_fields` per kind; the first
  row is always the kind toggle. `send::send_with_auth` branches on the kind:
  bearer/apikey resolve their secret and send (no token cache, no 401 retry),
  oauth2 keeps the cache-and-retry path.
- **Secrets are plaintext, but can be indirected.** Secret fields
  (`token`, `client_secret`) are saved as-is in the collection JSON (explicit
  user choice). To avoid that, a field may hold a single `$(…)` command
  substitution — `http::resolve_secret` runs it through `sh -c` at send time
  and uses the trimmed stdout. Only a value that is *entirely* `$(…)` is
  executed, never one embedded in a longer string. The in-memory `OAuthToken`
  is never persisted.
- **Tags become sidebar groups**, first tag only; untagged requests land in
  a `default` group. This wasn't asked for explicitly but was cheap and
  matches how most specs are organized.
- **`Method::parse`, not `FromStr`** — deliberately not the trait, to dodge
  a clippy lint; nothing else depends on `FromStr`.
- **Vim modes are `Normal` / `Insert` / `Command` / `Search`** — no Visual
  mode. Insert mode is reused for both single-line field edits (`LineEdit`)
  and the body `TextArea`; `app.editing` discriminates which. `Search` is the
  `/` sidebar filter: it re-applies on every keystroke, and `app.filter` (the
  committed query) is deliberately separate from `app.search` (the live
  prompt buffer) so `Esc` can drop the prompt without touching the filter.
- **Sidebar labels are a view concern.** `SavedRequest` keeps `name` (user
  editable), `summary` and `operation_id` (verbatim from the spec);
  `Collection.label_mode` picks which one renders. `:rename-all` is the only
  thing that overwrites `name`. Import prefers `summary` over `operationId`
  for the initial name — most real specs put a generated controller method
  name in `operationId`.
- **Switching collections reassigns the whole `App`** (`switch_collection` does
  `*self = App::new(...)`). Everything view-related is derived from the
  collection, so there's nothing to migrate by hand — and dropping the mpsc
  channel and cached `OAuthToken` is a feature, not collateral: a response still
  in flight for the previous collection can no longer land in the new one, and
  the token belonged to the old `auth` config. It deliberately does *not* go
  through a `pending_*` field like `run_external_edit` does; that indirection
  only exists because the editor needs the `&mut Terminal` to suspend raw mode,
  and routing a switch through the run loop would put it out of reach of
  `input_tests.rs`.
- **Pasting a URL rewrites collection state.** `App::apply_url_input` +
  `http::url_input` split an absolute URL into origin / path / query: the origin
  is added to `servers` (deduped on `trim_end_matches('/')`, since `E`-added
  servers may carry a trailing slash) **and made active**, because the URL bar
  renders `base_url() + path` and would otherwise show something other than what
  was just pasted. Query rows are replaced only when the input actually contained
  a `?` — otherwise fixing a typo'd path would silently wipe the disabled
  optional params an import set up. Two traps the module exists to handle:
  `Url::parse("localhost:8080/x")` *succeeds* with scheme `localhost` (hence the
  http/https + host check), and `{`/`}` are in the crate's path encode set, so
  `/pets/{id}` comes back as `/pets/%7Bid%7D` and needs `restore_braces`.
- **`path_params` follows the path.** `SavedRequest::sync_path_params` derives
  the rows from `{placeholders}` in `path`, pruning ones that no longer appear —
  `build_url` ignores those anyway, so a stale row only makes the Params tab
  lie. `{{variables}}` are skipped by the scanner. Import does *not* call it:
  spec-declared rows are authoritative there.
- **`$EDITOR` integration** shells out synchronously, suspending raw mode
  around it (`app::run_external_edit`). It writes/reads a temp file rather
  than piping, so it works with any editor.
- **Highlighting is hand-rolled** (`highlight.rs`), line-oriented, and never
  drops input: every character comes back out in some span (there's a test).
  A `syntect`-class dependency would be larger than the rest of the binary,
  and JSON/XML/plain is all a request client shows.
- **The body has two renderers.** `tui-textarea` styles whole lines only, so
  the Body tab renders a highlighted `Paragraph` in Normal mode and the raw
  `TextArea` in Insert mode. The textarea stays the source of truth either
  way; the read-only view scrolls by moving *its* cursor, which is why `j`/`k`
  on the Body tab drive `CursorMove`.
- **Dynamic variables live in the `{{…}}` namespace**, not a second syntax:
  `{{uuid}}`, `{{randomInt(1,10)}}`, `{{isoTimestamp}}`. A collection variable
  shadows a dynamic one of the same name (`{{$name}}` forces the dynamic one),
  so a fixed `uuid` can be pinned for debugging. Randomness comes from UUID v4
  bytes and the RFC 3339 formatter is hand-written — both to avoid adding
  `rand`/`chrono` for a handful of values.
- **Collections carry a saved view.** `last_request` (request id),
  `last_focus` (pane `1`/`2`/`3`) and `last_tab` are written by
  `App::record_view` when `:w` runs, and replayed by `App::new` — it reopens
  the request (expanding its group if `groups_collapsed` would hide it), then
  restores the pane and tab. Two deliberate wrinkles: the view is recorded
  *during* `save` rather than by `select_request`, because marking the
  collection dirty just for moving the sidebar cursor would make `:q` nag
  after a read-only browse (the trade: navigate away, quit clean, and the old
  position stays); and a saved `Response` focus falls back to the editor,
  since responses aren't persisted and pane 3 is empty on open.
  `Focus`/`EditorTab` stay in `app.rs` and gained serde derives, so `model.rs`
  reaches back into `app` for those two types.
- **CLI names resolve by slug** (`store::match_name`): the file is named after
  `slugify(name)` anyway, so `delete "some api"` and `delete some-api` both hit
  `Some API`. Exact name wins first. `store::resolve_collection` is the entry
  point and produces the "Available: …" error every name-taking command shares.
- **`cielago edit` edits a temp copy, not the file.** It only writes back after
  the edited text parses as a `Collection`; on a parse error the temp file is
  left in place and its path printed, so a botched edit is recoverable. A `name`
  changed in the editor is a rename (file moves, `config.last_collection`
  follows), and it bails rather than overwriting a different collection whose
  name slugifies the same.
- **`FieldDoc`s are stored on the request**, not read from the spec on demand:
  a collection's `spec_source` is often a URL that has moved or needs auth by
  the time the collection is opened. Cost is a re-import to refresh them, and
  older collections having none.

## Before committing

```sh
cargo fmt
cargo clippy --all-targets   # keep this clean, no warnings
cargo test
```

## Known gaps (intentionally out of scope for v1)

Swagger 2.0, interactive OAuth flows (auth-code / device / implicit — only
client-credentials is automated; bearer and API-key are static),
collection folders beyond tag grouping, request history/response diffing.
The Docs tab covers request inputs only — response schemas and status codes
aren't imported.
