use std::collections::BTreeMap;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::process::Command as ProcessCommand;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use inquire::{Confirm, Select, Text};

use cielago::app;
use cielago::model::{
    AuthKind, AuthStyle, Collection, DEFAULT_API_KEY_HEADER, LabelMode, OAuthConfig,
};
use cielago::openapi;
use cielago::store::{self, AppConfig};

const BANNER: &str = r#"

     c i e l a g o
                      
        .-'"/'.       
     _-"   (   '-_     
 _.-'       )     "-._ 
         .-'          `
____________ _____  __
"#;

#[derive(Parser)]
#[command(
    name = "cielago",
    version,
    about = "Like Postman but it actually works",
    before_help = BANNER
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Import an OpenAPI 3.x spec (JSON/YAML, file path or URL) as a collection
    Import {
        /// File path or http(s) URL of the spec
        source: String,
        /// Collection name (defaults to the spec's info.title)
        #[arg(long)]
        name: Option<String>,
        /// Skip interactive setup; keep spec-derived auth/servers and defaults
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Create an empty collection and open it in the TUI
    New {
        /// Collection name
        name: String,
        /// Base URL to start with (becomes the active server)
        #[arg(long, short)]
        server: Option<String>,
        /// Skip interactive setup; open straight into the TUI
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// List saved collections
    List {
        /// Show servers, request counts and file paths
        #[arg(short, long)]
        long: bool,
    },
    /// Open a collection in the TUI (defaults to the last opened one)
    Open { name: Option<String> },
    /// Delete a saved collection
    Delete {
        name: String,
        /// Skip the confirmation prompt
        #[arg(short, long)]
        force: bool,
    },
    /// Edit a collection's JSON in $EDITOR
    Edit { name: String },
    /// Rename a collection (renames its file too)
    Rename { name: String, new_name: String },
    /// Replace a collection's requests from a spec, keeping auth/vars/servers
    Update {
        /// Collection to update
        name: String,
        /// File path or http(s) URL of the spec to pull routes from
        source: String,
    },
    /// Show details about a collection
    Info { name: String },
    /// Print the path of a collection's JSON file
    Path { name: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Import { source, name, yes }) => cmd_import(&source, name, yes).await,
        Some(Command::New { name, server, yes }) => cmd_new(&name, server, yes).await,
        Some(Command::List { long }) => cmd_list(long),
        Some(Command::Open { name }) => cmd_open(name).await,
        Some(Command::Delete { name, force }) => cmd_delete(&name, force),
        Some(Command::Edit { name }) => cmd_edit(&name),
        Some(Command::Rename { name, new_name }) => cmd_rename(&name, &new_name),
        Some(Command::Update { name, source }) => cmd_update(&name, &source).await,
        Some(Command::Info { name }) => cmd_info(&name),
        Some(Command::Path { name }) => cmd_path(&name),
        None => cmd_open(None).await,
    }
}

async fn cmd_import(source: &str, name: Option<String>, yes: bool) -> Result<()> {
    let doc = openapi::load_spec(source).await?;
    let name = name
        .or_else(|| {
            doc.pointer("/info/title")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "imported".to_string());

    // Walk the user through the name, auth, server and display preferences
    // unless they opted out with `--yes` or stdin isn't a terminal (e.g. a
    // script or pipe), in which case the spec-derived defaults stand.
    let interactive = !yes && io::stdin().is_terminal();
    let name = if interactive {
        println!("{BANNER}");
        Text::new("Collection name").with_default(&name).prompt()?
    } else {
        name
    };

    let mut collection = openapi::import_spec(&doc, &name, Some(source.to_string()));

    if interactive {
        run_walkthrough(&mut collection)?;
    }

    let path = store::save_collection(&collection)?;

    println!(
        "Imported collection \"{}\" -> {}",
        collection.name,
        path.display()
    );
    println!("  {} requests", collection.requests.len());
    if collection.servers.is_empty() {
        println!("  servers: (none)");
    } else {
        println!(
            "  servers: {} (active: {})",
            collection.servers.join(", "),
            collection
                .base_url()
                .unwrap_or(collection.servers[0].as_str())
        );
    }
    match &collection.auth {
        Some(auth) => println!("  auth: {}", auth_summary(auth)),
        None => println!("  auth: none"),
    }
    Ok(())
}

/// One-line description of a configured auth scheme for the import summary.
fn auth_summary(auth: &OAuthConfig) -> String {
    match auth.kind {
        AuthKind::Bearer => {
            let state = if auth.token.is_empty() {
                " (no token set — set it with A in the TUI)"
            } else {
                ""
            };
            format!("bearer{state}")
        }
        AuthKind::ApiKey => {
            let state = if auth.token.is_empty() {
                " (no value set — set it with A in the TUI)"
            } else {
                ""
            };
            format!("api key in {}{state}", auth.api_key_header())
        }
        AuthKind::Oauth2 => {
            let state = if auth.client_id.is_empty() {
                " (set client id/secret with A in the TUI)"
            } else {
                ""
            };
            format!("oauth2 client-credentials, token url {}{state}", auth.token_url)
        }
    }
}

/// A free-text prompt whose empty submission — or an Esc — comes back as an
/// empty string, the "leave it blank, fill in from the TUI later" case.
fn optional_text(message: &str, help: &str) -> Result<String> {
    Ok(Text::new(message)
        .with_help_message(help)
        .prompt_skippable()?
        .unwrap_or_default())
}

/// A free-text prompt that offers `current` as its default (kept on an empty
/// reply). With no current value it behaves like a plain optional prompt.
fn text_default(message: &str, current: &str) -> Result<String> {
    if current.is_empty() {
        return optional_text(message, "blank to set later");
    }
    Ok(Text::new(message).with_default(current).prompt()?)
}

/// The shared auth → server → preferences walkthrough, run by both `import`
/// and `new` once the caller has confirmed a terminal and printed the banner.
/// Kept separate from the banner and name prompts, which differ per command.
fn run_walkthrough(collection: &mut Collection) -> Result<()> {
    prompt_auth(collection)?;
    prompt_server(collection)?;
    prompt_preferences(collection)?;
    Ok(())
}

/// Choose the collection's auth scheme, then collect that scheme's values. Any
/// value may be left blank and filled in later from the TUI. A scheme the spec
/// implied (e.g. oauth2 from a `clientCredentials` flow) is pre-selected and
/// seeds the oauth2 field prompts.
fn prompt_auth(collection: &mut Collection) -> Result<()> {
    const NONE: &str = "none";
    const BEARER: &str = "bearer token";
    const API_KEY: &str = "api key";
    const OAUTH2: &str = "oauth2 client-credentials";

    let cursor = match collection.auth.as_ref().map(|a| a.kind) {
        Some(AuthKind::Bearer) => 1,
        Some(AuthKind::ApiKey) => 2,
        Some(AuthKind::Oauth2) => 3,
        None => 0,
    };
    let choice = Select::new("Authentication", vec![NONE, BEARER, API_KEY, OAUTH2])
        .with_starting_cursor(cursor)
        .prompt()?;

    collection.auth = match choice {
        BEARER => Some(OAuthConfig {
            kind: AuthKind::Bearer,
            token: optional_text("Bearer token", "blank to set later")?,
            ..Default::default()
        }),
        API_KEY => Some(OAuthConfig {
            kind: AuthKind::ApiKey,
            header: Text::new("Header name")
                .with_default(DEFAULT_API_KEY_HEADER)
                .prompt()?,
            token: optional_text("API key value", "blank to set later")?,
            ..Default::default()
        }),
        OAUTH2 => {
            // Reuse spec-derived token url/scopes as defaults when present.
            let mut cfg = collection.auth.clone().unwrap_or_default();
            cfg.kind = AuthKind::Oauth2;
            cfg.token_url = text_default("Token URL", &cfg.token_url)?;
            cfg.client_id = optional_text("Client id", "blank to set later")?;
            cfg.client_secret = optional_text("Client secret", "blank to set later")?;
            let scopes = text_default("Scopes (space-separated)", &cfg.scopes.join(" "))?;
            cfg.scopes = scopes.split_whitespace().map(String::from).collect();
            let style =
                Select::new("Send credentials via", vec!["basic header", "form body"]).prompt()?;
            cfg.auth_style = if style == "form body" {
                AuthStyle::Post
            } else {
                AuthStyle::Basic
            };
            Some(cfg)
        }
        _ => None,
    };
    Ok(())
}

/// Pick the active base URL. Spec-derived servers are offered in a list with a
/// trailing "enter a new URL" escape; a typed URL is added and made active.
fn prompt_server(collection: &mut Collection) -> Result<()> {
    if collection.servers.is_empty() {
        let url = normalize_server(&optional_text("Base URL", "blank for none")?);
        if !url.is_empty() {
            collection.servers.push(url);
            collection.active_server = 0;
        }
        return Ok(());
    }

    const NEW: &str = "＋ enter a new URL…";
    let mut options: Vec<String> = collection.servers.clone();
    options.push(NEW.to_string());
    let choice = Select::new("Active server", options).prompt()?;

    if choice == NEW {
        let url = normalize_server(&optional_text("Base URL", "blank to keep the first")?);
        collection.active_server = if url.is_empty() {
            0
        } else {
            collection
                .servers
                .iter()
                .position(|s| *s == url)
                .unwrap_or_else(|| {
                    collection.servers.push(url);
                    collection.servers.len() - 1
                })
        };
    } else {
        collection.active_server = collection
            .servers
            .iter()
            .position(|s| *s == choice)
            .unwrap_or(0);
    }
    Ok(())
}

/// Trim surrounding whitespace and a trailing slash so a typed URL matches the
/// form imported servers are stored in (and doesn't duplicate one).
fn normalize_server(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

/// Collection display preferences: how the sidebar labels requests, and whether
/// tag groups start collapsed.
fn prompt_preferences(collection: &mut Collection) -> Result<()> {
    let label = Select::new("Label requests by", vec!["name", "summary", "path"]).prompt()?;
    collection.label_mode = match label {
        "summary" => LabelMode::Summary,
        "path" => LabelMode::Path,
        _ => LabelMode::Name,
    };

    collection.groups_collapsed = Confirm::new("Collapse tag groups on open?")
        .with_default(true)
        .prompt()?;
    Ok(())
}

/// Create an empty collection and drop straight into the TUI to fill it in.
/// The existence check is on the slug path rather than via
/// `store::resolve_collection`, which bails by contract on a name that doesn't
/// exist yet — and the path check also catches names that collide after
/// slugify, same as `cielago rename`.
async fn cmd_new(name: &str, server: Option<String>, yes: bool) -> Result<()> {
    let path = store::collection_path(name)?;
    if path.exists() {
        bail!(
            "a collection already exists at {} — open it with `cielago open {name:?}` or pick another name",
            path.display()
        );
    }

    let mut collection = Collection::new(name);
    if let Some(url) = server {
        // Trailing slash trimmed to match imported servers, so pasting a URL in
        // the TUI later recognises this one instead of adding a duplicate.
        let url = url.trim().trim_end_matches('/').to_string();
        if !url.is_empty() {
            collection.servers.push(url);
        }
    }

    // Same auth/server/preferences walkthrough as import, so a hand-made
    // collection starts configured rather than blank. Skipped with `--yes` or
    // when stdin isn't a terminal; either way the TUI opens next to fill in the
    // rest. A `--server` given on the command line seeds the server prompt.
    if !yes && io::stdin().is_terminal() {
        println!("{BANNER}");
        run_walkthrough(&mut collection)?;
    }

    let path = store::save_collection(&collection)?;
    println!(
        "Created collection \"{}\" -> {}",
        collection.name,
        path.display()
    );

    let mut config = AppConfig::load();
    config.last_collection = Some(collection.name.clone());
    let _ = config.save();
    app::run(collection, path, config).await
}

fn cmd_list(long: bool) -> Result<()> {
    let names = store::list_collections()?;
    if names.is_empty() {
        println!(
            "No collections yet. Import one: cielago import <spec>\n\
             …or start from scratch:      cielago new <name>"
        );
        return Ok(());
    }
    let last = AppConfig::load().last_collection;
    for n in names {
        if !long {
            println!("{n}");
            continue;
        }
        let marker = if last.as_deref() == Some(n.as_str()) {
            "*"
        } else {
            " "
        };
        let path = store::collection_path(&n)?;
        match store::load_collection(&n) {
            Ok(c) => println!(
                "{marker} {n}\n    {} requests, {} server(s){}\n    {}",
                c.requests.len(),
                c.servers.len(),
                if c.auth.is_some() { ", oauth2" } else { "" },
                path.display()
            ),
            Err(e) => println!("{marker} {n}\n    unreadable: {e}\n    {}", path.display()),
        }
    }
    Ok(())
}

fn cmd_delete(name: &str, force: bool) -> Result<()> {
    let name = store::resolve_collection(name)?;
    let collection = store::load_collection(&name).ok();
    let path = store::collection_path(&name)?;

    if !force {
        let count = collection
            .as_ref()
            .map(|c| format!(" ({} requests)", c.requests.len()))
            .unwrap_or_default();
        print!("Delete collection \"{name}\"{count}? [y/N] ");
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes" | "Yes") {
            println!("Aborted.");
            return Ok(());
        }
    }

    store::delete_collection(&name)?;
    let mut config = AppConfig::load();
    if config.last_collection.as_deref() == Some(name.as_str()) {
        config.last_collection = None;
        let _ = config.save();
    }
    println!("Deleted \"{name}\" ({})", path.display());
    Ok(())
}

/// Edit the collection JSON in `$EDITOR`. The edit happens on a temp copy so a
/// file that no longer parses never replaces the saved one; a `name` changed in
/// the editor moves the file, same as `cielago rename`.
fn cmd_edit(name: &str) -> Result<()> {
    let name = store::resolve_collection(name)?;
    let path = store::collection_path(&name)?;
    let original =
        fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;

    let mut tmp = std::env::temp_dir();
    tmp.push(format!(
        "cielago-{}-{}.json",
        store::slugify(&name),
        std::process::id()
    ));
    fs::write(&tmp, &original)?;

    let editor = AppConfig::load().editor_cmd();
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vi");
    let status = ProcessCommand::new(program)
        .args(parts)
        .arg(&tmp)
        .status()
        .with_context(|| format!("launching editor {editor:?}"))?;
    if !status.success() {
        let _ = fs::remove_file(&tmp);
        bail!("editor exited with {status}; collection left unchanged");
    }

    let edited = fs::read_to_string(&tmp)?;
    if edited == original {
        let _ = fs::remove_file(&tmp);
        println!("No changes.");
        return Ok(());
    }

    let collection: Collection = match serde_json::from_str(&edited) {
        Ok(c) => c,
        Err(e) => bail!(
            "edited JSON is not a valid collection: {e}\n\nYour edits are kept at {}; the saved collection is unchanged.",
            tmp.display()
        ),
    };
    let new_path = store::collection_path(&collection.name)?;
    if new_path != path && new_path.exists() {
        bail!(
            "renaming to {:?} would overwrite the collection at {}.\n\nYour edits are kept at {}; the saved collection is unchanged.",
            collection.name,
            new_path.display(),
            tmp.display()
        );
    }
    let _ = fs::remove_file(&tmp);

    store::save_collection(&collection)?;
    if new_path != path {
        fs::remove_file(&path).ok();
        update_last_collection(&name, &collection.name);
        println!(
            "Saved \"{}\" -> {} (was \"{name}\")",
            collection.name,
            new_path.display()
        );
    } else {
        println!("Saved \"{}\" -> {}", collection.name, new_path.display());
    }
    Ok(())
}

fn cmd_rename(name: &str, new_name: &str) -> Result<()> {
    let name = store::resolve_collection(name)?;
    let mut collection = store::load_collection(&name)?;
    let old_path = store::collection_path(&name)?;
    let new_path = store::collection_path(new_name)?;

    if new_path != old_path && new_path.exists() {
        bail!(
            "a collection already exists at {} — pick another name",
            new_path.display()
        );
    }

    collection.name = new_name.to_string();
    store::save_collection(&collection)?;
    if new_path != old_path {
        fs::remove_file(&old_path).ok();
    }
    update_last_collection(&name, new_name);
    println!(
        "Renamed \"{name}\" -> \"{new_name}\" ({})",
        new_path.display()
    );
    Ok(())
}

/// Refresh a collection's routes from a spec without touching the rest of it.
/// Only `requests` is replaced (existing routes are overwritten); auth,
/// variables, servers, active server and view state stay as the user left
/// them. `last_request` is cleared because re-import mints new request ids, so
/// the old pointer would dangle.
async fn cmd_update(name: &str, source: &str) -> Result<()> {
    let name = store::resolve_collection(name)?;
    let mut collection = store::load_collection(&name)?;

    let doc = openapi::load_spec(source).await?;
    // Import under the collection's own name so the throwaway result matches;
    // only its `requests` are pulled across.
    let imported = openapi::import_spec(&doc, &collection.name, Some(source.to_string()));

    let before = collection.requests.len();
    let after = imported.requests.len();
    collection.replace_requests_from(imported);
    collection.spec_source = Some(source.to_string());

    let path = store::save_collection(&collection)?;
    println!(
        "Updated collection \"{}\" -> {}",
        collection.name,
        path.display()
    );
    println!("  {before} -> {after} requests");
    Ok(())
}

fn cmd_info(name: &str) -> Result<()> {
    let name = store::resolve_collection(name)?;
    let collection = store::load_collection(&name)?;
    let path = store::collection_path(&name)?;

    println!("{}", collection.name);
    println!("  file:      {}", path.display());
    if let Some(src) = &collection.spec_source {
        println!("  spec:      {src}");
    }
    if collection.servers.is_empty() {
        println!("  servers:   (none)");
    } else {
        for (i, s) in collection.servers.iter().enumerate() {
            let marker = if i == collection.active_server {
                "*"
            } else {
                " "
            };
            println!("  server{marker}   {s}");
        }
    }
    println!("  requests:  {}", collection.requests.len());
    println!("  variables: {}", collection.variables.len());
    match &collection.auth {
        Some(auth) => println!(
            "  auth:      oauth2 client-credentials, token url {} ({} client id)",
            auth.token_url,
            if auth.client_id.is_empty() {
                "no"
            } else {
                "has"
            }
        ),
        None => println!("  auth:      none"),
    }

    let mut groups: BTreeMap<&str, usize> = BTreeMap::new();
    for r in &collection.requests {
        *groups
            .entry(r.tags.first().map(String::as_str).unwrap_or("default"))
            .or_default() += 1;
    }
    if !groups.is_empty() {
        println!("  groups:");
        for (group, count) in groups {
            println!("    {group} ({count})");
        }
    }
    Ok(())
}

fn cmd_path(name: &str) -> Result<()> {
    let name = store::resolve_collection(name)?;
    println!("{}", store::collection_path(&name)?.display());
    Ok(())
}

/// Keep `config.last_collection` pointing at a collection that was renamed.
fn update_last_collection(old: &str, new: &str) {
    let mut config = AppConfig::load();
    if config.last_collection.as_deref() == Some(old) {
        config.last_collection = Some(new.to_string());
        let _ = config.save();
    }
}

async fn cmd_open(name: Option<String>) -> Result<()> {
    let mut config = AppConfig::load();
    let name = match name.or_else(|| config.last_collection.clone()) {
        Some(n) => n,
        None => {
            let names = store::list_collections()?;
            match names.as_slice() {
                [] => bail!(
                    "No collections yet. Import one first:\n\n  cielago import <spec.json|yaml|url>\n\nOr create an empty one:\n\n  cielago new <name>"
                ),
                [only] => only.clone(),
                many => bail!(
                    "Multiple collections exist; choose one:\n\n  cielago open <name>\n\nAvailable: {}",
                    many.join(", ")
                ),
            }
        }
    };

    let collection =
        store::load_collection(&name).with_context(|| format!("loading collection {name:?}"))?;
    config.last_collection = Some(collection.name.clone());
    let _ = config.save();
    let path = store::collection_path(&collection.name)?;
    app::run(collection, path, config).await
}
