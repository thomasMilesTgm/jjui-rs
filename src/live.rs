use crate::repository::{Repository, Snapshot};
use anyhow::Result;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
    time::{Duration, Instant},
};

pub enum Update {
    Repository(Snapshot),
    Files(Vec<String>),
    Error(String),
}

pub struct Live {
    pub updates: Receiver<Update>,
    pub refresh: SyncSender<()>,
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

pub fn start(path: PathBuf, limit: usize) -> Result<Live> {
    let (send, updates) = mpsc::sync_channel(32);
    let (refresh, requests) = mpsc::sync_channel(1);
    // Initialize before entering raw mode, so startup errors are readable.
    let mut repo = Repository::open(&path, limit)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let first = runtime
        .block_on(repo.refresh(true))?
        .expect("initial snapshot");
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
    thread::spawn(move || {
        let _watcher = watcher;
        let mut last_check = Instant::now();
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
    Ok(Live { updates, refresh })
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
