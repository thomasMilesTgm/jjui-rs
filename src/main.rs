mod live;
mod repository;
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
    about = "A live Jujutsu repository browser. Reads jj-lib directly; never spawns jj."
)]
struct Args {
    /// Workspace directory (or a subdirectory)
    #[arg(short = 'R', long, default_value = ".")]
    repository: PathBuf,
    /// Maximum visible revisions
    #[arg(long, default_value = "200", value_parser = clap::value_parser!(u32).range(1..=10000))]
    limit: u32,
    /// Print one repository snapshot as JSON without entering the TUI
    #[arg(long, conflicts_with = "watch_json")]
    once: bool,
    /// Stream live updates as newline-delimited JSON without entering the TUI
    #[arg(long)]
    watch_json: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.once {
        let mut repo = repository::Repository::open(&args.repository, args.limit as usize)?;
        let runtime = tokio::runtime::Runtime::new()?;
        println!(
            "{}",
            serde_json::to_string(&runtime.block_on(repo.refresh(true))?)?
        );
        return Ok(());
    }
    if !args.watch_json && (!io::stdin().is_terminal() || !io::stdout().is_terminal()) {
        bail!("The TUI requires a terminal; use --once or --watch-json for noninteractive output");
    }
    let live = live::start(args.repository, args.limit as usize)?;
    if args.watch_json {
        for update in &live.updates {
            let value = match update {
                live::Update::Repository(snapshot) => {
                    serde_json::json!({"type": "repository", "snapshot": snapshot})
                }
                live::Update::Files(paths) => serde_json::json!({"type": "files", "paths": paths}),
                live::Update::Error(message) => {
                    serde_json::json!({"type": "error", "message": message})
                }
            };
            println!("{value}");
            io::stdout().flush()?;
        }
        Ok(())
    } else {
        ui::run(live)
    }
}
