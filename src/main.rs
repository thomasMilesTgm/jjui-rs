mod browser;
mod commands;
mod graph;
mod input;
mod keybinds;
mod live;
mod operations;
mod preview;
mod repository;
mod settings;
mod snapshot;
mod ui;

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
    /// Explicit keybinds.toml (otherwise global and repository files are layered)
    #[arg(long)]
    keybinds: Option<PathBuf>,
    /// Validate bindings and print effective action bindings
    #[arg(long)]
    check_keybinds: bool,
    /// Revision expression evaluated directly through jj-lib
    #[arg(short = 'r', long, default_value = "all()")]
    revisions: String,
    /// Automatically snapshot working-tree edits through jj-lib (writes operations)
    #[arg(long)]
    auto_snapshot: bool,
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
    let keys = keybinds::Keybinds::load(&args.repository, args.keybinds.as_deref())?;
    if args.check_keybinds {
        println!("{}", keys.help());
        return Ok(());
    }
    if let Some(id) = &args.diff {
        let mut repo = repository::Repository::open(&args.repository, args.limit as usize)?;
        let runtime = tokio::runtime::Runtime::new()?;
        if args.auto_snapshot {
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
        let runtime = tokio::runtime::Runtime::new()?;
        if args.auto_snapshot {
            runtime.block_on(repo.snapshot())?;
        }
        println!(
            "{}",
            serde_json::to_string(&runtime.block_on(repo.set_revset(args.revisions.clone()))?)?
        );
        return Ok(());
    }
    if !args.watch_json && (!io::stdin().is_terminal() || !io::stdout().is_terminal()) {
        bail!("The TUI requires a terminal; use --once or --watch-json for noninteractive output");
    }
    let live = live::start(
        args.repository,
        args.limit as usize,
        args.auto_snapshot,
        args.revisions,
    )?;
    if args.watch_json {
        for update in &live.updates {
            let value = match update {
                live::Update::OperationDiff { id, result } => {
                    serde_json::json!({"type":"operation_diff","id":id,"result":result})
                }
                live::Update::Listing(result) => {
                    serde_json::json!({"type":"listing","result":result})
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
        ui::run(live, keys)
    }
}
