use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::process::Command as ProcessCommand;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use cielago::app;
use cielago::model::Collection;
use cielago::openapi;
use cielago::store::{self, AppConfig};

#[derive(Parser)]
#[command(
    name = "cielago",
    version,
    about = "A Postman-like TUI driven by OpenAPI collections"
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
    },
    /// Create an empty collection and open it in the TUI
    New {
        /// Collection name
        name: String,
        /// Base URL to start with (becomes the active server)
        #[arg(long, short)]
        server: Option<String>,
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
    /// Show details about a collection
    Info { name: String },
    /// Print the path of a collection's JSON file
    Path { name: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Import { source, name }) => cmd_import(&source, name).await,
        Some(Command::New { name, server }) => cmd_new(&name, server).await,
        Some(Command::List { long }) => cmd_list(long),
        Some(Command::Open { name }) => cmd_open(name).await,
        Some(Command::Delete { name, force }) => cmd_delete(&name, force),
        Some(Command::Edit { name }) => cmd_edit(&name),
        Some(Command::Rename { name, new_name }) => cmd_rename(&name, &new_name),
        Some(Command::Info { name }) => cmd_info(&name),
        Some(Command::Path { name }) => cmd_path(&name),
        None => cmd_open(None).await,
    }
}

async fn cmd_import(source: &str, name: Option<String>) -> Result<()> {
    let doc = openapi::load_spec(source).await?;
    let name = name
        .or_else(|| {
            doc.pointer("/info/title")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "imported".to_string());

    let collection = openapi::import_spec(&doc, &name, Some(source.to_string()));
    let path = store::save_collection(&collection)?;

    println!(
        "Imported collection \"{}\" -> {}",
        collection.name,
        path.display()
    );
    println!("  {} requests", collection.requests.len());
    if !collection.servers.is_empty() {
        println!("  servers: {}", collection.servers.join(", "));
    }
    if let Some(auth) = &collection.auth {
        println!(
            "  oauth2 client-credentials: {} (set client id/secret with A in the TUI)",
            auth.token_url
        );
    }
    Ok(())
}

/// Create an empty collection and drop straight into the TUI to fill it in.
/// The existence check is on the slug path rather than via
/// `store::resolve_collection`, which bails by contract on a name that doesn't
/// exist yet — and the path check also catches names that collide after
/// slugify, same as `cielago rename`.
async fn cmd_new(name: &str, server: Option<String>) -> Result<()> {
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
