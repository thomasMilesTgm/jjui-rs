use crate::repository::{Repository, Snapshot};
use anyhow::Result;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, PartialEq)]
pub enum Request {
    Workspaces,
    Plugin(crate::plugins::Task),
    Chunks { id: String, path: String },
    ApplyChunks(crate::chunks::Request),
    Revset(String),
    Listing(crate::browser::Query),
    OperationDiff(String),
    Command(crate::commands::CommandSpec),
}

pub enum Update {
    Workspaces(Result<Vec<crate::workspaces::Entry>, String>),
    PluginFinished(Result<String, String>),
    Chunks {
        id: String,
        path: String,
        result: Result<crate::chunks::Session, String>,
    },
    OperationDiff {
        id: String,
        result: Result<String, String>,
    },
    Listing(Result<crate::browser::Listing, String>),
    CommandFinished {
        command: String,
        result: Result<String, String>,
    },
    RevsetApplied(String),
    Repository(Snapshot),
    Preview {
        id: String,
        result: Result<crate::preview::Preview, String>,
    },
    Files(Vec<String>),
    Error(String),
}

pub struct Live {
    pub repo_path: PathBuf,
    pub limit: usize,
    pub auto_snapshot: bool,
    pub updates: Receiver<Update>,
    pub tasks: SyncSender<Request>,
    pub root: PathBuf,
    pub initial_revset: String,
    pub refresh: SyncSender<()>,
    pub preview: SyncSender<String>,
}

fn worktree_path(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    if relative
        .components()
        .any(|c| matches!(c.as_os_str().to_str(), Some(".jj" | ".git" | "target")))
    {
        return None;
    }
    Some(relative.display().to_string())
}

pub fn start(
    path: PathBuf,
    limit: usize,
    auto_snapshot: bool,
    initial_revset: String,
) -> Result<Live> {
    let (tasks, task_requests) = mpsc::sync_channel(8);
    let (send, updates) = mpsc::sync_channel(32);
    let (preview, preview_requests) = mpsc::sync_channel::<String>(1);
    let (refresh, requests) = mpsc::sync_channel(1);
    // Initialize before entering raw mode, so startup errors are readable.
    let mut repo = Repository::open(&path, limit)?;
    let runtime = tokio::runtime::Runtime::new()?;
    if auto_snapshot {
        runtime.block_on(repo.snapshot())?;
    }
    let first = runtime.block_on(repo.set_revset(initial_revset.clone()))?;
    send.send(Update::Repository(first))?;
    let root = repo.root();
    let metadata = repo.repo_path();
    let (events_send, events) = mpsc::sync_channel(1024);
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |event| {
        // A full queue already guarantees a refresh; never block the OS watcher.
        let _ = events_send.try_send(event);
    })?;
    watcher.watch(&root, RecursiveMode::Recursive)?;
    if !metadata.starts_with(&root) {
        watcher.watch(&metadata, RecursiveMode::Recursive)?;
    }
    let live_root = root.clone();
    thread::spawn(move || {
        let _watcher = watcher;
        let mut last_check = Instant::now();
        let mut last_snapshot = Instant::now();
        let mut retry = false;
        loop {
            let mut changed = Vec::new();
            let mut dirty = false;
            let mut record = |event: notify::Result<Event>| match event {
                Ok(event) if !matches!(event.kind, EventKind::Access(_)) => {
                    dirty = true;
                    for path in event.paths {
                        if let Some(path) = worktree_path(&root, &path) {
                            changed.push(path);
                        }
                    }
                }
                Err(error) => {
                    let _ = send.try_send(Update::Error(format!("Watcher: {error}")));
                }
                _ => {}
            };
            match events.recv_timeout(Duration::from_millis(100)) {
                Ok(event) => {
                    record(event);
                    // Fixed batching window keeps continuous writes from starving refreshes.
                    thread::sleep(Duration::from_millis(75));
                    for event in events.try_iter().take(1024) {
                        record(event);
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            let force = match requests.try_recv() {
                Ok(()) => true,
                Err(mpsc::TryRecvError::Empty) => false,
                Err(mpsc::TryRecvError::Disconnected) => break,
            };
            let mut force = force;
            if auto_snapshot
                && (!changed.is_empty() || last_snapshot.elapsed() >= Duration::from_secs(2))
            {
                last_snapshot = Instant::now();
                match runtime.block_on(repo.snapshot()) {
                    Ok(()) => {}
                    Err(error) => {
                        let _ = send.send(Update::Error(format!("Snapshot: {error:#}")));
                    }
                }
            }
            if let Ok(request) = task_requests.try_recv() {
                let update = match request {
                    Request::Workspaces => {
                        let result = (|| -> anyhow::Result<_> {
                            repo.reload_workspace()?;
                            if let Some(snapshot) = runtime.block_on(repo.refresh(true))? {
                                let _ = send.send(Update::Repository(snapshot));
                            }
                            runtime.block_on(repo.workspaces())
                        })();
                        Update::Workspaces(result.map_err(|e| format!("{e:#}")))
                    }
                    Request::Chunks { id, path } => Update::Chunks {
                        result: runtime
                            .block_on(repo.chunks(&id, &path))
                            .map_err(|e| format!("{e:#}")),
                        id,
                        path,
                    },
                    Request::Plugin(task) => {
                        let result = match task {
                            crate::plugins::Task::ReadDiff(id) => runtime
                                .block_on(repo.preview(&id))
                                .map(|preview| {
                                    preview
                                        .files
                                        .iter()
                                        .map(|file| format!("{}\n{}", file.path, file.patch))
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                })
                                .map_err(|error| format!("{error:#}")),
                            crate::plugins::Task::Command(args) => {
                                let result = crate::commands::execute(
                                    &root,
                                    &crate::commands::CommandSpec::new(args),
                                )
                                .map_err(|e| format!("{e:#}"));
                                if let Ok(Some(snapshot)) = runtime.block_on(repo.refresh(true)) {
                                    let _ = send.send(Update::Repository(snapshot));
                                }
                                result
                            }
                            crate::plugins::Task::Revset(expression) => {
                                match runtime.block_on(repo.set_revset(expression.clone())) {
                                    Ok(snapshot) => {
                                        let _ = send.send(Update::Repository(snapshot));
                                        let _ =
                                            send.send(Update::RevsetApplied(expression.clone()));
                                        Ok(expression)
                                    }
                                    Err(error) => Err(format!("{error:#}")),
                                }
                            }
                            crate::plugins::Task::Refresh => {
                                match runtime.block_on(repo.refresh(true)) {
                                    Ok(snapshot) => {
                                        if let Some(snapshot) = snapshot {
                                            let _ = send.send(Update::Repository(snapshot));
                                        }
                                        Ok(String::new())
                                    }
                                    Err(error) => Err(format!("{error:#}")),
                                }
                            }
                        };
                        Update::PluginFinished(result)
                    }
                    Request::ApplyChunks(request) => {
                        let result =
                            crate::chunks::apply(&root, &request).map_err(|e| format!("{e:#}"));
                        force = true;
                        Update::CommandFinished {
                            command: format!("{:?} selected hunks", request.action),
                            result,
                        }
                    }
                    Request::Revset(expression) => {
                        match runtime.block_on(repo.set_revset(expression.clone())) {
                            Ok(snapshot) => {
                                if send.send(Update::Repository(snapshot)).is_err() {
                                    break;
                                }
                                Update::RevsetApplied(expression)
                            }
                            Err(error) => Update::Error(format!("{error:#}")),
                        }
                    }
                    Request::OperationDiff(id) => Update::OperationDiff {
                        result: runtime
                            .block_on(repo.operation_diff(&id))
                            .map_err(|e| format!("{e:#}")),
                        id,
                    },
                    Request::Listing(query) => Update::Listing(
                        runtime
                            .block_on(repo.listing(query))
                            .map_err(|e| format!("{e:#}")),
                    ),
                    Request::Command(spec) => {
                        let result =
                            crate::commands::execute(&root, &spec).map_err(|e| format!("{e:#}"));
                        if spec.args.first().is_some_and(|arg| arg == "workspace")
                            && let Err(error) = repo.reload_workspace()
                        {
                            let _ = send.send(Update::Error(format!("{error:#}")));
                        }
                        force = true;
                        Update::CommandFinished {
                            command: spec.display(),
                            result,
                        }
                    }
                };
                if send.send(update).is_err() {
                    break;
                }
            }
            // Cheap operation-head reconciliation covers dropped filesystem events.
            if dirty || force || last_check.elapsed() >= Duration::from_secs(2) {
                let result = runtime.block_on(repo.refresh(force || retry));
                retry = result.is_err();
                last_check = Instant::now();
                let update = match result {
                    Ok(Some(snapshot)) => Some(Update::Repository(snapshot)),
                    Ok(None) => None,
                    Err(error) => Some(Update::Error(format!("{error:#}"))),
                };
                if let Some(update) = update
                    && send.send(update).is_err()
                {
                    break;
                }
            }
            if let Ok(id) = preview_requests.try_recv() {
                let result = runtime
                    .block_on(repo.preview(&id))
                    .map_err(|error| format!("{error:#}"));
                if send.send(Update::Preview { id, result }).is_err() {
                    break;
                }
            }
            if !changed.is_empty() {
                changed.sort();
                changed.dedup();
                changed.truncate(100);
                if send.send(Update::Files(changed)).is_err() {
                    break;
                }
            }
        }
    });
    Ok(Live {
        repo_path: metadata,
        limit,
        auto_snapshot,
        updates,
        root: live_root,
        initial_revset,
        tasks,
        refresh,
        preview,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_is_not_reported_as_user_edits() {
        let root = Path::new("/repo");
        assert_eq!(
            worktree_path(root, Path::new("/repo/src/main.rs")),
            Some("src/main.rs".into())
        );
        for path in [
            "/repo/.jj/repo/op_heads/a",
            "/repo/.git/HEAD",
            "/repo/target/a",
            "/elsewhere/a",
        ] {
            assert_eq!(worktree_path(root, Path::new(path)), None);
        }
    }
}
