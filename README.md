# cielago

![cover](https://files.stevedylan.dev/cielago-demo.jpg)

A vim-style TUI for building and sending HTTP requests, organized into
collections you can import straight from an OpenAPI spec.

## Features

- **OpenAPI 3.x import** — turn a spec (file or URL) into a collection with
  requests, params, example bodies, servers, and docs prefilled.
- **Ad-hoc collections** — no spec needed; paste a full URL and it's split into
  server, path, and query params for you.
- **Vim-style TUI** — three panes (requests / editor / response), `j`/`k`
  navigation, `:` command line, `/` incremental search.
- **Variables** — `{{name}}` from the collection, plus dynamic ones like
  `{{uuid}}`, `{{timestamp}}`, `{{randomInt(1,100)}}`.
- **OAuth2 client credentials** — per collection, tokens cached in memory only.
- **Server switcher** — swap base URLs so requests stay portable across envs.
- **Syntax highlighting** — JSON and XML in both request bodies and responses.
- **Plain JSON storage** — collections live in `~/.config/cielago/collections/`.

## Installation

```sh
cargo install --path .
```

Or build without installing:

```sh
cargo build --release
./target/release/cielago --help
```

## Usage

```sh
cielago                          # open the last-used collection in the TUI
cielago open [name]              # open a specific collection
cielago import <spec|url>        # import an OpenAPI 3.x spec
cielago new <name> [--server u]  # create an empty collection and open it
cielago list [-l]                # list collections (-l adds counts + paths)
cielago info <name>              # servers, counts, auth, groups
cielago edit <name>              # edit the collection JSON in $EDITOR
cielago rename <name> <new>      # rename a collection and its file
cielago delete <name> [-f]       # delete a collection
cielago path <name>              # print the collection's JSON path
```

`<name>` matches loosely — `Some API`, `some api`, and `some-api` all resolve to
the same collection.

### Keys

| Key | Action |
|---|---|
| `1`/`2`/`3`, `Tab` | Focus sidebar / editor / response |
| `z` | Maximize focused pane |
| `[` / `]` | Previous / next editor tab |
| `Enter` | Send request |
| `/` | Search requests |
| `E` / `A` | Servers / OAuth config |
| `:` | Command line (`:w`, `:q`, `:new`, `:open`, …) |
| `?` | Help |

Sidebar: `n`/`r`/`d`/`y` new/rename/delete/duplicate, `t` cycle label source.
Tables: `space` toggle row, `i` edit, `a` add, `d` delete, `m` cycle method,
`p` edit URL. Body/response: `i` edit inline, `e` open in `$EDITOR`,
`j`/`k`/`d`/`u`/`g`/`G` scroll.

Press `?` in the TUI for the full list.

## Development

```sh
cargo build
cargo test
cargo clippy --all-targets
cargo fmt
```

## License

[MIT](LICENSE)
