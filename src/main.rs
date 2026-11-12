mod bookmarks;
mod browser;
mod chunks;
mod commands;
mod config;
mod elision;
mod graph;
mod input;
mod keybinds;
mod live;
mod operations;
mod plugins;
mod preview;
mod repository;
mod settings;
mod snapshot;
mod theme;
mod ui;
mod watching;
mod workspaces;

use anyhow::{Result, bail};
use clap::Parser;
use std::{
    io::{self, IsTerminal, Write},
    path::PathBuf,
};

#[derive(Parser)]
#[command(
    version,
    about = "A live Jujutsu repository browser. Live reads use jj-lib; explicit editing actions use jj."
)]
struct Args {
    /// Workspace directory (or a subdirectory)
    #[arg(short = 'R', long, default_value = ".")]
    repository: PathBuf,
    /// Explicit config.toml (otherwise global and workspace files are layered)
    #[arg(long)]
    config: Option<PathBuf>,
    /// Validate and print effective application settings without opening the TUI
    #[arg(long)]
    check_config: bool,
    /// Explicit keybinds.toml (otherwise global and repository files are layered)
    #[arg(long)]
    keybinds: Option<PathBuf>,
    /// Plugin manifest (defaults to the global jjui-rs/plugins.toml)
    #[arg(long)]
    plugins: Option<PathBuf>,
    /// Compile plugins and validate their entrypoints without running them
    #[arg(long)]
    check_plugins: bool,
    /// Theme: default, dark, light, or a theme.toml path
    #[arg(long)]
    theme: Option<String>,
    /// Validate and print the effective theme
    #[arg(long)]
    check_theme: bool,
    #[arg(long, hide = true, num_args = 3)]
    chunk_tool: Option<Vec<PathBuf>>,
    #[arg(long, hide = true, num_args = 2)]
    chunks: Option<Vec<String>>,
    #[arg(long, hide = true)]
    apply_hunks: Option<PathBuf>,
    /// Validate bindings and print effective action bindings
    #[arg(long)]
    check_keybinds: bool,
    /// Revision expression (defaults to Jujutsu's revsets.log)
    #[arg(short = 'r', long)]
    revisions: Option<String>,
    /// Keep snapshotting edits through jj-lib after the TUI opening snapshot
    #[arg(long, conflicts_with = "no_auto_snapshot")]
    auto_snapshot: bool,
    /// Disable ongoing automatic snapshots (the TUI still snapshots once when opened)
    #[arg(long)]
    no_auto_snapshot: bool,
    /// Maximum visible revisions
    #[arg(long, default_value = "200", value_parser = clap::value_parser!(u32).range(1..=10000))]
    limit: u32,
    /// Print one repository snapshot as JSON without entering the TUI
    #[arg(long, conflicts_with = "watch_json")]
    once: bool,
    /// Stream live updates as newline-delimited JSON without entering the TUI
    #[arg(long)]
    watch_json: bool,
    /// Print recorded file diffs for a full commit ID as JSON
    #[arg(long, conflicts_with_all = ["once", "watch_json"])]
    diff: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if let Some(paths) = args.chunk_tool {
        return chunks::edit(&paths[0], &paths[1], &paths[2]);
    }
    if let Some(path) = args.apply_hunks {
        let request = serde_json::from_slice(&std::fs::read(path)?)?;
        println!("{}", chunks::apply(&args.repository, &request)?);
        return Ok(());
    }
    if let Some(values) = args.chunks {
        let mut repo = repository::Repository::open(&args.repository, args.limit as usize)?;
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(repo.refresh(true))?;
        println!(
            "{}",
            serde_json::to_string(&runtime.block_on(repo.chunks(&values[0], &values[1]))?)?
        );
        return Ok(());
    }
    let config_source = config::Source {
        path: args.config,
        auto_snapshot: if args.auto_snapshot {
            Some(true)
        } else if args.no_auto_snapshot {
            Some(false)
        } else {
            None
        },
    };
    let config = config_source.load(&args.repository)?;
    if args.check_config {
        println!("{}", toml::to_string_pretty(&config)?);
        return Ok(());
    }
    let theme = theme::Theme::load(&args.repository, args.theme.as_deref())?;
    if args.check_theme {
        println!("{}", toml::to_string_pretty(&theme)?);
        return Ok(());
    }
    let plugins = plugins::Config::load(args.plugins.as_deref())?;
    if args.check_plugins {
        for name in plugins::check(plugins)? {
            println!("plugin.{name}");
        }
        return Ok(());
    }
    let keys = keybinds::Keybinds::load_with_plugins(
        &args.repository,
        args.keybinds.as_deref(),
        &plugins.names(),
    )?;
    if args.check_keybinds {
        println!("{}", keys.help());
        return Ok(());
    }
    if let Some(id) = &args.diff {
        let mut repo = repository::Repository::open(&args.repository, args.limit as usize)?;
        let runtime = tokio::runtime::Runtime::new()?;
        if config.auto_snapshot {
            runtime.block_on(repo.snapshot())?;
        }
        runtime.block_on(repo.refresh(true))?;
        println!(
            "{}",
            serde_json::to_string(&runtime.block_on(repo.preview(id))?)?
        );
        return Ok(());
    }
    if args.once {
        let mut repo = repository::Repository::open(&args.repository, args.limit as usize)?;
        let revisions = args.revisions.unwrap_or_else(|| repo.revset().to_owned());
        let runtime = tokio::runtime::Runtime::new()?;
        if config.auto_snapshot {
            runtime.block_on(repo.snapshot())?;
        }
        println!(
            "{}",
            serde_json::to_string(&runtime.block_on(repo.set_revset(revisions))?)?
        );
        return Ok(());
    }
    if !args.watch_json && (!io::stdin().is_terminal() || !io::stdout().is_terminal()) {
        bail!("The TUI requires a terminal; use --once or --watch-json for noninteractive output");
    }
    let live = live::start(
        args.repository,
        args.limit as usize,
        config.auto_snapshot,
        args.revisions,
        !args.watch_json,
    )?;
    if args.watch_json {
        for update in &live.updates {
            let value = match update {
                live::Update::Workspaces(result) => {
                    serde_json::json!({"type":"workspaces","result":result})
                }
                live::Update::PluginFinished(result) => {
                    serde_json::json!({"type":"plugin","result":result})
                }
                live::Update::Chunks { id, path, result } => {
                    serde_json::json!({"type":"chunks","id":id,"path":path,"result":result})
                }
                live::Update::OperationDiff { id, result } => {
                    serde_json::json!({"type":"operation_diff","id":id,"result":result})
                }
                live::Update::Listing(result) => {
                    serde_json::json!({"type":"listing","result":result})
                }
                live::Update::CommandOutput(chunk) => {
                    serde_json::json!({"type":"command_output","chunk":chunk})
                }
                live::Update::CommandFinished { command, result } => {
                    serde_json::json!({"type":"command","command":command,"result":result})
                }
                live::Update::RevsetApplied(expression) => {
                    serde_json::json!({"type":"revset","expression":expression})
                }
                live::Update::Repository(snapshot) => {
                    serde_json::json!({"type": "repository", "snapshot": snapshot})
                }
                live::Update::Files(paths) => serde_json::json!({"type": "files", "paths": paths}),
                live::Update::Preview { id, result } => {
                    serde_json::json!({"type": "preview", "id": id, "result": result})
                }
                live::Update::Error(message) => {
                    serde_json::json!({"type": "error", "message": message})
                }
            };
            println!("{value}");
            io::stdout().flush()?;
        }
        Ok(())
    } else {
        ui::run(
            live,
            keys,
            theme,
            args.theme,
            plugins,
            args.keybinds,
            config_source,
        )
    }
}
