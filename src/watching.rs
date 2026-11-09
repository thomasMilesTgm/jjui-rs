//! Watch relevant directories without traversing ignored build output or object stores.
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    thread,
    time::Duration,
};

pub struct Guard(Arc<AtomicBool>);
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

pub fn start(
    root: PathBuf,
    metadata: PathBuf,
    events: SyncSender<notify::Result<Event>>,
) -> notify::Result<Guard> {
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let (send, receive) = mpsc::sync_channel(1024);
    let overflow = Arc::new(AtomicBool::new(false));
    let dropped = overflow.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
        if event
            .as_ref()
            .is_ok_and(|event| matches!(event.kind, EventKind::Access(_)))
        {
            return;
        }
        if send.try_send(event).is_err() {
            dropped.store(true, Ordering::Relaxed);
        }
    })?;
    // This one cheap registration prevents losing edits made immediately after
    // the first screen appears. Descendant discovery happens off the UI thread.
    watcher.watch(&root, RecursiveMode::NonRecursive)?;
    thread::spawn(move || {
        let mut watched = HashSet::from([root.clone()]);
        let mut rescan = true;
        while !stopping.load(Ordering::Relaxed) {
            if rescan || overflow.swap(false, Ordering::Relaxed) {
                if let Err(error) = refresh(&mut watcher, &mut watched, &root, &metadata, &stopping)
                {
                    let _ = events.try_send(Err(error));
                }
                rescan = false;
            }
            match receive.recv_timeout(Duration::from_millis(100)) {
                Ok(event) => {
                    if let Ok(event) = &event {
                        rescan = needs_rescan(event, &watched);
                    }
                    if matches!(
                        events.try_send(event),
                        Err(mpsc::TrySendError::Disconnected(_))
                    ) {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    });
    Ok(Guard(stop))
}

fn needs_rescan(event: &Event, watched: &HashSet<PathBuf>) -> bool {
    !matches!(event.kind, EventKind::Access(_))
        && event.paths.iter().any(|path| {
            path.file_name().is_some_and(|name| {
                name == ".gitignore" || name == "exclude" || name == "tree_state"
            }) || (matches!(
                event.kind,
                EventKind::Create(_)
                    | EventKind::Remove(_)
                    | EventKind::Modify(notify::event::ModifyKind::Name(_))
            ) && (path.is_dir() || watched.contains(path)))
        })
}

fn refresh(
    watcher: &mut RecommendedWatcher,
    watched: &mut HashSet<PathBuf>,
    root: &Path,
    metadata: &Path,
    stopping: &AtomicBool,
) -> notify::Result<()> {
    let mut wanted = HashSet::new();
    // Register the root before walking, so new directories created during the
    // scan produce an event and get picked up by the next scan.
    let metadata_dirs = [
        root.to_owned(),
        root.join(".jj"),
        root.join(".jj/working_copy"),
        metadata.to_owned(),
        metadata.join("op_heads"),
        metadata.join("op_heads/heads"),
    ];
    // Ignore rules don't untrack files. Keep watches for tracked files even
    // when their parent directories now match an ignore pattern.
    let tracked = crate::repository::Repository::open(root, 1)
        .and_then(|repo| repo.tracked_directories())
        .map_err(|error| notify::Error::generic(&format!("Read tracked directories: {error:#}")))?;
    for path in metadata_dirs
        .into_iter()
        .chain(tracked)
        .chain(directories(root))
    {
        if stopping.load(Ordering::Relaxed) {
            return Ok(());
        }
        if !path.is_dir() {
            continue;
        }
        if !watched.contains(&path) {
            match watcher.watch(&path, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    watched.insert(path.clone());
                }
                Err(_) if !path.exists() => continue,
                Err(error) => return Err(error),
            }
        }
        wanted.insert(path);
    }
    watched.retain(|path| {
        if wanted.contains(path) {
            true
        } else {
            let _ = watcher.unwatch(path);
            false
        }
    });
    Ok(())
}

fn directories(root: &Path) -> impl Iterator<Item = PathBuf> + use<> {
    ignore::WalkBuilder::new(root)
        .hidden(false)
        .ignore(false)
        .require_git(false)
        .follow_links(false)
        .filter_entry(|entry| {
            entry.depth() == 0 || !matches!(entry.file_name().to_str(), Some(".git" | ".jj"))
        })
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_dir()))
        .map(|entry| entry.into_path())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ignored_output_and_metadata_are_not_walked_but_hidden_sources_are() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        for name in [
            "src/nested",
            "target/deep",
            "node_modules/deep",
            ".hidden/src",
            ".git/objects/deep",
            ".jj/repo/store/deep",
        ] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        std::fs::write(root.join(".gitignore"), "target/\nnode_modules/\n").unwrap();
        let paths: HashSet<_> = directories(root)
            .map(|p| p.strip_prefix(root).unwrap().to_owned())
            .collect();
        assert!(paths.contains(Path::new("src/nested")));
        assert!(paths.contains(Path::new(".hidden/src")));
        assert!(!paths.iter().any(|p| {
            ["target", "node_modules", ".git", ".jj"]
                .iter()
                .any(|name| p.starts_with(name))
        }));
    }
}
