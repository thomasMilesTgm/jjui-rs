//! Only explicit user actions enter this module. Repository polling never does.
use anyhow::{Context, Result, bail};
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq)]
pub struct CommandSpec {
    pub args: Vec<String>,
    pub interactive: bool,
}
impl CommandSpec {
    pub fn new(args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            args: args.into_iter().map(Into::into).collect(),
            interactive: false,
        }
    }
    pub fn interactive(mut self) -> Self {
        self.interactive = true;
        self
    }
    pub fn display(&self) -> String {
        format!("jj {}", shell_words::join(&self.args))
    }
    pub fn validate(&self) -> Result<()> {
        let Some(command) = self.args.first() else {
            bail!("Enter a jj command");
        };
        // Deliberately reject aliases/global-option prefixes: they could invoke log
        // or change the repository. All commands are scoped to the open workspace.
        if !matches!(
            command.as_str(),
            "new"
                | "workspace"
                | "describe"
                | "edit"
                | "rebase"
                | "squash"
                | "abandon"
                | "absorb"
                | "duplicate"
                | "split"
                | "diffedit"
                | "commit"
                | "bookmark"
                | "undo"
                | "redo"
                | "git"
                | "operation"
                | "evolog"
                | "restore"
                | "file"
                | "status"
                | "util"
                | "revert"
                | "resolve"
        ) {
            bail!(
                "Unsupported command {command:?}; revision history is read through jj-lib, never jj log"
            );
        }
        if self.args.iter().any(|arg| {
            arg == "-R"
                || arg.starts_with("--repository")
                || arg.starts_with("--config")
                || arg.starts_with("-R")
        }) {
            bail!("Repository/config overrides are not allowed in the command prompt");
        }
        Ok(())
    }
    pub fn process(&self, root: &Path) -> Result<Command> {
        self.validate()?;
        let mut command = Command::new("jj");
        command
            .current_dir(root)
            .arg("--no-pager")
            .args(["--color", "never"])
            .args(&self.args);
        Ok(command)
    }
}

fn collect(mut input: impl Read) -> String {
    let mut output = Vec::new();
    let mut buffer = [0; 8192];
    let mut truncated = false;
    while let Ok(count) = input.read(&mut buffer) {
        if count == 0 {
            break;
        }
        let keep = count.min((2 * 1024 * 1024usize).saturating_sub(output.len()));
        output.extend_from_slice(&buffer[..keep]);
        truncated |= keep < count;
    }
    let mut text: String = String::from_utf8_lossy(&output)
        .chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                '�'
            } else {
                c
            }
        })
        .collect();
    if truncated {
        text.push_str("\n[output truncated at 2 MiB]");
    }
    text
}

pub fn execute(root: &Path, spec: &CommandSpec) -> Result<String> {
    execute_process(spec.process(root)?, &spec.display())
}

pub(crate) fn execute_process(mut process: Command, display: &str) -> Result<String> {
    let mut child = process
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Could not run jj; install jj 0.46 for editing and Git actions")?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let out = thread::spawn(move || collect(stdout));
    let err = thread::spawn(move || collect(stderr));
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() > Duration::from_secs(120) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Command timed out after 120 seconds: {}", display);
        }
        thread::sleep(Duration::from_millis(30));
    };
    let output = format!(
        "{}{}",
        out.join().unwrap_or_default(),
        err.join().unwrap_or_default()
    );
    if !status.success() {
        bail!("{} failed ({status})\n{output}", display);
    }
    Ok(if output.is_empty() {
        format!("{} completed", display)
    } else {
        output
    })
}

/// All user strings are separate argv entries, never shell interpolation.
pub fn action(
    action: &str,
    sources: &[String],
    value: &str,
    files: &[String],
) -> Result<CommandSpec> {
    let source = sources.first().map(String::as_str).unwrap_or("@");
    let union = sources.join(" | ");
    let mut spec = match action {
        "new" => CommandSpec::new(std::iter::once("new".to_owned()).chain(sources.iter().cloned())),
        "describe" => CommandSpec::new([
            "describe".to_owned(),
            "-r".into(),
            union.clone(),
            format!("--message={value}"),
        ]),
        "describe_editor" => CommandSpec::new(["describe", "-r", source]).interactive(),
        "edit" => CommandSpec::new(["edit", source]),
        "rebase" => CommandSpec::new(["rebase", "-r", &union, "-o", value]),
        "squash" => CommandSpec::new([
            "squash",
            "--from",
            &union,
            "--into",
            value,
            "--use-destination-message",
        ]),
        "abandon" => CommandSpec::new(["abandon", &union]),
        "absorb" => CommandSpec::new(["absorb", "--from", source]),
        "duplicate" => CommandSpec::new(["duplicate", &union]),
        "split" if files.is_empty() => CommandSpec::new(["split", "-r", source]).interactive(),
        "split" => CommandSpec::new([
            "split".to_owned(),
            "-r".into(),
            source.into(),
            format!("--message={value}"),
        ]),
        "diff_edit" => CommandSpec::new(["diffedit", "-r", source]).interactive(),
        "commit" => CommandSpec::new(["commit".to_owned(), format!("--message={value}")]),
        "restore" => CommandSpec::new(["restore", "--changes-in", source]),
        "set_bookmark" => CommandSpec::new([
            "bookmark",
            "set",
            "-r",
            source,
            "--",
            &serde_json::to_string(value)?,
        ]),
        "bookmark_delete" => CommandSpec::new(["bookmark", "delete", "--", value]),
        "bookmark_track" => CommandSpec::new(["bookmark", "track", "--", value]),
        "bookmark_untrack" => CommandSpec::new(["bookmark", "untrack", "--", value]),
        "undo" => CommandSpec::new(["undo"]),
        "redo" => CommandSpec::new(["redo"]),
        "snapshot" => CommandSpec::new(["util", "snapshot"]),
        "operation_restore" => CommandSpec::new(["operation", "restore", value]),
        "evolution" => CommandSpec::new([
            "evolog",
            "-r",
            source,
            "--limit",
            "200",
            "--ignore-working-copy",
        ]),
        "fetch" => CommandSpec::new(["git", "fetch", "--remote", value]),
        "push" => {
            let mut spec = CommandSpec::new(["git", "push", "--remote", value]);
            if !union.is_empty() {
                spec.args.extend(["--revisions".into(), union.clone()]);
            }
            spec
        }
        "remote_remove" => CommandSpec::new(["git", "remote", "remove", "--", value]),
        "remote_add" => {
            let words = shell_words::split(value)?;
            if words.len() != 2 {
                bail!("Enter remote name and URL, quoted if needed");
            }
            CommandSpec::new(["git", "remote", "add", "--", &words[0], &words[1]])
        }
        "command" => {
            let mut spec = CommandSpec::new(shell_words::split(value)?);
            let command = spec.args.first().map(String::as_str).unwrap_or("");
            let message = spec.args.iter().any(|a| {
                a == "-m"
                    || a == "--message"
                    || a.starts_with("--message=")
                    || a == "--use-destination-message"
                    || a == "-u"
            });
            spec.interactive = matches!(command, "diffedit" | "resolve")
                || matches!(command, "describe" | "commit" | "split" | "squash") && !message;
            spec
        }
        _ => bail!("Unknown action {action}"),
    };
    if matches!(action, "restore" | "split" | "squash" | "absorb") && !files.is_empty() {
        spec.args.push("--".into());
        for path in files {
            spec.args
                .push(format!("root-file:{}", serde_json::to_string(path)?));
        }
    }
    spec.validate()?;
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_subprocesses_and_repository_redirects_are_rejected() {
        for args in [
            vec!["log"],
            vec!["--config", "aliases.x=['log']", "x"],
            vec!["new", "-R", "/other"],
        ] {
            assert!(CommandSpec::new(args).validate().is_err());
        }
        assert!(
            CommandSpec::new(["describe", "-m", "text; $(nothing)"])
                .validate()
                .is_ok()
        );
    }
    fn fixture() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        execute(
            temp.path(),
            &CommandSpec::new(["git", "init", "--colocate"]),
        )
        .unwrap();
        std::fs::write(
            temp.path().join(".jj/repo/config.toml"),
            "[user]\nname='Test'\nemail='test@example.org'\n",
        )
        .unwrap();
        temp
    }
    fn current(root: &Path) -> crate::repository::Revision {
        let mut repo = crate::repository::Repository::open(root, 100).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime
            .block_on(repo.refresh(true))
            .unwrap()
            .unwrap()
            .revisions
            .into_iter()
            .find(|r| r.working_copy)
            .unwrap()
    }
    fn run_action(root: &Path, name: &str, sources: &[String], value: &str, files: &[String]) {
        let spec = action(name, sources, value, files).unwrap();
        run_spec(root, &spec).unwrap_or_else(|e| panic!("{}: {e:#}", spec.display()));
    }
    fn run_spec(root: &Path, spec: &CommandSpec) -> Result<String> {
        let mut process = spec.process(root).unwrap();
        process
            .env("JJ_CONFIG", "")
            .env("XDG_CONFIG_HOME", root.join(".jj/test-config"));
        execute_process(process, &spec.display())
    }
    #[test]
    fn editing_actions_preserve_literal_descriptions_and_selected_sources() {
        let temp = fixture();
        let root = temp.path();
        let first = current(root).change_id;
        run_action(
            root,
            "describe",
            std::slice::from_ref(&first),
            "literal ; $(no-shell)\nsecond line",
            &[],
        );
        assert_eq!(
            current(root).description,
            "literal ; $(no-shell)\nsecond line\n"
        );
        run_action(
            root,
            "set_bookmark",
            std::slice::from_ref(&first),
            "base",
            &[],
        );
        run_action(root, "new", std::slice::from_ref(&first), "", &[]);
        let child = current(root).change_id;
        assert_ne!(child, first);
        run_action(root, "describe", std::slice::from_ref(&child), "child", &[]);
        run_action(root, "duplicate", std::slice::from_ref(&child), "", &[]);
        run_action(root, "rebase", std::slice::from_ref(&child), &first, &[]);
        run_action(root, "abandon", std::slice::from_ref(&child), "", &[]);
        run_action(root, "undo", &[], "", &[]);
        run_action(root, "redo", &[], "", &[]);
        run_action(root, "bookmark_delete", &[], "base", &[]);
    }

    #[test]
    fn related_operations_follow_rewrites_and_refs_but_exclude_other_changes() {
        fn history(root: &Path, query: crate::browser::Query) -> Vec<crate::browser::Row> {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let mut repo = crate::repository::Repository::open(root, 100).unwrap();
            runtime.block_on(repo.refresh(true)).unwrap();
            runtime.block_on(repo.listing(query)).unwrap().rows
        }
        let temp = fixture();
        let root = temp.path();
        let first = current(root);
        let sources = [first.change_id.clone()];
        run_action(root, "describe", &sources, "first", &[]);
        let describe = history(root, crate::browser::Query::Operations)[0]
            .id
            .clone();
        run_action(root, "set_bookmark", &sources, "topic", &[]);
        let bookmark = history(root, crate::browser::Query::Operations)[0]
            .id
            .clone();
        run_action(root, "new", &sources, "", &[]);
        let child = current(root);
        run_action(root, "describe", &[child.change_id], "unrelated child", &[]);
        let unrelated = history(root, crate::browser::Query::Operations)[0]
            .id
            .clone();
        let related = history(
            root,
            crate::browser::Query::CommitOperations(first.commit_id),
        );
        assert!(related.iter().any(|row| row.id == describe));
        assert!(related.iter().any(|row| row.id == bookmark));
        assert!(!related.iter().any(|row| row.id == unrelated));
        assert!(
            related
                .iter()
                .all(|row| !row.operation.as_ref().unwrap().current)
        );
        // Removed intervening operations are contracted into real ancestry.
        let described = related.iter().position(|row| row.id == describe).unwrap();
        let bookmarked = related.iter().position(|row| row.id == bookmark).unwrap();
        assert!(bookmarked < described);
        assert!(
            related[bookmarked]
                .operation
                .as_ref()
                .unwrap()
                .parents
                .contains(&describe)
        );
        let after = history(root, crate::browser::Query::Operations);
        assert_eq!(
            after[0].id, unrelated,
            "history reads must not create operations"
        );
        assert!(after[0].operation.as_ref().unwrap().current);
        assert!(after.last().unwrap().operation.as_ref().unwrap().root);
    }

    #[test]
    fn snapshot_operation_diff_compares_recorded_workspace_trees() {
        let temp = fixture();
        let root = temp.path();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut repo = crate::repository::Repository::open(root, 100).unwrap();
        std::fs::write(root.join("changed.txt"), "before\n").unwrap();
        std::fs::write(
            root.join("unchanged.txt"),
            "not part of the second snapshot\n",
        )
        .unwrap();
        std::fs::write(root.join("deleted.txt"), "removed\n").unwrap();
        run_action(root, "snapshot", &[], "", &[]);
        let before = runtime.block_on(repo.refresh(true)).unwrap().unwrap();
        std::fs::write(root.join("changed.txt"), "after\n").unwrap();
        std::fs::remove_file(root.join("deleted.txt")).unwrap();
        std::fs::write(root.join("added.txt"), "added\n").unwrap();
        run_action(root, "snapshot", &[], "", &[]);
        let snapshot = runtime.block_on(repo.refresh(true)).unwrap().unwrap();
        let listing = runtime
            .block_on(repo.listing(crate::browser::Query::Operations))
            .unwrap();
        assert!(listing.rows[0].operation.as_ref().unwrap().snapshot);
        // A later snapshot must not change a historical operation's patch.
        std::fs::write(root.join("changed.txt"), "later\n").unwrap();
        run_action(root, "snapshot", &[], "", &[]);
        let latest = runtime.block_on(repo.refresh(true)).unwrap().unwrap();
        let patch = runtime
            .block_on(repo.operation_diff(&snapshot.operation))
            .unwrap();
        assert!(patch.contains(&format!("Parent {}", before.operation)));
        assert!(patch.contains("-before\n+after\n"));
        assert!(patch.contains("+added\n"));
        assert!(patch.contains("-removed\n"));
        assert!(!patch.contains("unchanged.txt"));
        assert!(!patch.contains("later"));
        assert_eq!(
            runtime
                .block_on(repo.refresh(true))
                .unwrap()
                .unwrap()
                .operation,
            latest.operation
        );
        run_action(
            root,
            "describe",
            &[current(root).change_id],
            "description only",
            &[],
        );
        let described = runtime.block_on(repo.refresh(true)).unwrap().unwrap();
        assert!(
            runtime
                .block_on(repo.operation_diff(&described.operation))
                .is_err()
        );
    }
    #[test]
    fn selected_file_restore_split_and_squash_use_literal_filesets() {
        let temp = fixture();
        let root = temp.path();
        let first = current(root).change_id;
        std::fs::write(root.join("a | b.txt"), "old\n").unwrap();
        run_action(root, "new", std::slice::from_ref(&first), "", &[]);
        let child = current(root).change_id;
        std::fs::write(root.join("a | b.txt"), "new\n").unwrap();
        std::fs::write(root.join("second.txt"), "second\n").unwrap();
        run_action(root, "describe", std::slice::from_ref(&child), "child", &[]);
        run_action(
            root,
            "restore",
            std::slice::from_ref(&child),
            "",
            &["a | b.txt".into()],
        );
        assert_eq!(
            std::fs::read_to_string(root.join("a | b.txt")).unwrap(),
            "old\n"
        );
        run_action(
            root,
            "split",
            std::slice::from_ref(&child),
            "split files",
            &["second.txt".into()],
        );
        let now = current(root).change_id;
        run_action(root, "squash", std::slice::from_ref(&now), &first, &[]);
    }
    #[test]
    fn bookmark_menu_moves_literal_names_and_inline_set_rejects_backwards() {
        let temp = fixture();
        let root = temp.path();
        let first = current(root);
        let name = "topic|special";
        run_action(
            root,
            "set_bookmark",
            std::slice::from_ref(&first.change_id),
            name,
            &[],
        );
        run_action(
            root,
            "set_bookmark",
            std::slice::from_ref(&first.change_id),
            "other",
            &[],
        );
        run_action(root, "new", std::slice::from_ref(&first.change_id), "", &[]);
        let child = current(root);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let load = |id: String| {
            let mut repo = crate::repository::Repository::open(root, 100).unwrap();
            runtime.block_on(repo.refresh(true)).unwrap();
            runtime
                .block_on(repo.listing(crate::browser::Query::BookmarkOperations(id)))
                .unwrap()
        };
        let menu = load(child.commit_id.clone());
        let moving = menu
            .rows
            .iter()
            .filter_map(|row| row.bookmark.as_ref())
            .find(|action| action.kind == "move" && action.name == name)
            .unwrap();
        run_spec(root, &CommandSpec::new(moving.args.clone())).unwrap();
        let spec = action(
            "set_bookmark",
            std::slice::from_ref(&first.change_id),
            name,
            &[],
        )
        .unwrap();
        assert!(!spec.args.iter().any(|arg| arg == "--allow-backwards"));
        assert!(
            run_spec(root, &spec).is_err(),
            "B must not silently move a bookmark backwards"
        );
        let menu = load(first.commit_id.clone());
        let moving = menu
            .rows
            .iter()
            .filter_map(|row| row.bookmark.as_ref())
            .find(|action| action.kind == "move" && action.name == name)
            .unwrap();
        assert!(moving.args.iter().any(|arg| arg == "--allow-backwards"));
        run_spec(root, &CommandSpec::new(moving.args.clone())).unwrap();
        let menu = load(first.commit_id);
        assert!(
            !menu
                .rows
                .iter()
                .filter_map(|row| row.bookmark.as_ref())
                .any(|action| action.kind == "move" && action.name == name)
        );
        let deleting = menu
            .rows
            .iter()
            .filter_map(|row| row.bookmark.as_ref())
            .find(|action| action.kind == "delete" && action.name == name)
            .unwrap();
        run_spec(root, &CommandSpec::new(deleting.args.clone())).unwrap();
        let menu = load(child.commit_id);
        assert!(
            menu.rows
                .iter()
                .filter_map(|row| row.bookmark.as_ref())
                .any(|action| action.name == "other")
        );
        assert!(
            !menu
                .rows
                .iter()
                .filter_map(|row| row.bookmark.as_ref())
                .any(|action| action.name == name)
        );
    }

    #[test]
    fn divergent_remote_bookmarks_decorate_graph_and_exact_operation_boundary() {
        let temp = fixture();
        let root = temp.path();
        std::fs::write(root.join(".jj/repo/config.toml"),
            "[user]\nname='Test'\nemail='test@example.org'\n[revset-aliases]\n'immutable_heads()'='none()'\n").unwrap();
        let remote = tempfile::tempdir().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "--bare", "--quiet"])
                .arg(remote.path())
                .status()
                .unwrap()
                .success()
        );
        let source = current(root).change_id;
        run_action(
            root,
            "describe",
            std::slice::from_ref(&source),
            "published",
            &[],
        );
        run_action(
            root,
            "set_bookmark",
            std::slice::from_ref(&source),
            "main",
            &[],
        );
        run_action(
            root,
            "remote_add",
            &[],
            &format!(
                "origin {}",
                shell_words::quote(&remote.path().display().to_string())
            ),
            &[],
        );
        run_action(root, "bookmark_track", &[], "main@origin", &[]);
        run_action(root, "push", std::slice::from_ref(&source), "origin", &[]);
        let original = current(root).commit_id;
        assert_eq!(current(root).bookmarks, ["main"]);
        // An unrelated operation immediately before the snapshot is still the correct anchor.
        run_action(root, "set_bookmark", &["root()".into()], "unrelated", &[]);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut repo = crate::repository::Repository::open(root, 100).unwrap();
        let boundary = runtime
            .block_on(repo.refresh(true))
            .unwrap()
            .unwrap()
            .operation;
        std::fs::write(root.join("file"), "first snapshot").unwrap();
        run_action(root, "snapshot", &[], "", &[]);
        std::fs::write(root.join("file"), "second snapshot").unwrap();
        run_action(root, "snapshot", &[], "", &[]);
        let snapshot = runtime.block_on(repo.refresh(true)).unwrap().unwrap();
        let local = snapshot.revisions.iter().find(|r| r.working_copy).unwrap();
        assert!(
            local.bookmarks.contains(&"main*".into()),
            "{:?}",
            snapshot.revisions
        );
        assert!(
            snapshot
                .revisions
                .iter()
                .any(|r| r.commit_id == original && r.bookmarks.contains(&"main@origin".into()))
        );
        let operations = runtime
            .block_on(repo.listing(crate::browser::Query::CommitOperations(
                local.commit_id.clone(),
            )))
            .unwrap();
        let anchors: Vec<_> = operations
            .rows
            .iter()
            .filter(|row| {
                row.operation
                    .as_ref()
                    .unwrap()
                    .bookmarks
                    .contains(&"main@origin".into())
            })
            .collect();
        assert_eq!(anchors.len(), 1);
        assert_eq!(anchors[0].id, boundary);
        assert_ne!(operations.rows[0].id, boundary);
        run_action(root, "push", std::slice::from_ref(&source), "origin", &[]);
        let snapshot = runtime.block_on(repo.refresh(true)).unwrap().unwrap();
        assert_eq!(
            snapshot
                .revisions
                .iter()
                .find(|r| r.working_copy)
                .unwrap()
                .bookmarks,
            ["main"]
        );
        assert!(
            !snapshot
                .revisions
                .iter()
                .any(|r| r.bookmarks.contains(&"main@origin".into()))
        );
    }

    #[test]
    fn git_remote_roundtrip_and_native_listings() {
        let temp = fixture();
        let root = temp.path();
        let remote = tempfile::tempdir().unwrap();
        let status = Command::new("git")
            .args(["init", "--bare", "--quiet"])
            .arg(remote.path())
            .status()
            .unwrap();
        assert!(status.success());
        let source = current(root).change_id;
        run_action(
            root,
            "describe",
            std::slice::from_ref(&source),
            "publishable change",
            &[],
        );
        run_action(
            root,
            "set_bookmark",
            std::slice::from_ref(&source),
            "main",
            &[],
        );
        run_action(
            root,
            "remote_add",
            &[],
            &format!(
                "origin {}",
                shell_words::quote(&remote.path().display().to_string())
            ),
            &[],
        );
        run_action(root, "push", std::slice::from_ref(&source), "origin", &[]);
        run_action(root, "fetch", &[], "origin", &[]);
        run_action(root, "bookmark_untrack", &[], "main@origin", &[]);
        run_action(root, "bookmark_track", &[], "main@origin", &[]);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut repo = crate::repository::Repository::open(root, 100).unwrap();
        runtime.block_on(repo.refresh(true)).unwrap().unwrap();
        let bookmarks = runtime
            .block_on(repo.listing(crate::browser::Query::Bookmarks))
            .unwrap();
        assert!(bookmarks.rows.iter().any(|r| r.id == "main@origin"));
        let focused = current(root).commit_id;
        for kind in ["untrack", "track"] {
            runtime.block_on(repo.refresh(true)).unwrap();
            let menu = runtime
                .block_on(repo.listing(crate::browser::Query::BookmarkOperations(focused.clone())))
                .unwrap();
            assert!(menu.remotes.contains(&"origin".into()));
            let item = menu
                .rows
                .iter()
                .filter_map(|row| row.bookmark.as_ref())
                .find(|action| {
                    action.kind == kind
                        && action.name == "main"
                        && action.remote.as_deref() == Some("origin")
                })
                .unwrap_or_else(|| panic!("missing {kind}: {:?}", menu.rows));
            run_spec(root, &CommandSpec::new(item.args.clone())).unwrap();
        }
        let snapshot = runtime.block_on(repo.refresh(true)).unwrap().unwrap();
        let operations = runtime
            .block_on(repo.listing(crate::browser::Query::Operations))
            .unwrap();
        assert_eq!(operations.rows[0].id, snapshot.operation);
        assert!(operations.rows.len() > 3);
        run_action(root, "new", std::slice::from_ref(&source), "", &[]);
        run_action(root, "operation_restore", &[], &snapshot.operation, &[]);
        assert_eq!(current(root).change_id, source);
        run_action(root, "remote_remove", &[], "origin", &[]);
    }
}
