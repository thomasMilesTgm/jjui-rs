//! Hunk selection happens in the TUI. jj's diff-editor protocol applies the
//! selected tree and handles locks, immutable commits, rebases, and checkout.
use anyhow::{Context, Result, bail, ensure};
use futures::AsyncReadExt;
use jj_lib::{
    backend::{CommitId, MergedTreeValue, TreeValue},
    diff::{ContentDiff, DiffHunkKind},
    object_id::ObjectId,
    repo::{ReadonlyRepo, Repo},
    repo_path::RepoPath,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
    process::Command,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextFile {
    pub text: String,
    pub executable: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub commit_id: String,
    pub change_id: String,
    pub path: String,
    pub before: Option<TextFile>,
    pub after: Option<TextFile>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Action {
    Restore,
    Split,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub session: Session,
    pub selected: BTreeSet<usize>,
    pub action: Action,
    pub description: String,
}
#[derive(Clone, Debug)]
pub struct Hunk {
    pub patch: String,
}
impl Session {
    pub fn hunks(&self) -> Vec<Hunk> {
        let old = self.before.as_ref().map_or("", |f| f.text.as_str());
        let new = self.after.as_ref().map_or("", |f| f.text.as_str());
        let diff = ContentDiff::by_line([old, new]);
        let mut old_line = 1;
        let mut new_line = 1;
        let mut result = Vec::new();
        let segments: Vec<_> = diff.hunks().collect();
        for (index, hunk) in segments.iter().enumerate() {
            let a = std::str::from_utf8(hunk.contents[0].as_ref()).unwrap();
            let b = std::str::from_utf8(hunk.contents[1].as_ref()).unwrap();
            if hunk.kind == DiffHunkKind::Different {
                let preceding = index
                    .checked_sub(1)
                    .and_then(|i| segments.get(i))
                    .filter(|h| h.kind == DiffHunkKind::Matching)
                    .map(|h| std::str::from_utf8(h.contents[0].as_ref()).unwrap())
                    .unwrap_or("");
                let following = segments
                    .get(index + 1)
                    .filter(|h| h.kind == DiffHunkKind::Matching)
                    .map(|h| std::str::from_utf8(h.contents[0].as_ref()).unwrap())
                    .unwrap_or("");
                let context_before: Vec<_> = preceding.split_inclusive('\n').collect();
                let context_before = &context_before[context_before.len().saturating_sub(3)..];
                let context_after: Vec<_> = following.split_inclusive('\n').take(3).collect();
                let context_count = context_before.len() + context_after.len();
                let old_count = a.lines().count() + context_count;
                let new_count = b.lines().count() + context_count;
                let mut patch = format!(
                    "@@ -{},{} +{},{} @@\n",
                    if old_count == 0 {
                        old_line - 1
                    } else {
                        old_line - context_before.len()
                    },
                    old_count,
                    if new_count == 0 {
                        new_line - 1
                    } else {
                        new_line - context_before.len()
                    },
                    new_count
                );
                for (prefix, text) in [
                    (' ', context_before.concat()),
                    ('-', a.into()),
                    ('+', b.into()),
                    (' ', context_after.concat()),
                ] {
                    for line in text.split_inclusive('\n') {
                        patch.push(prefix);
                        patch.push_str(line);
                        if !line.ends_with('\n') {
                            patch.push_str("\n\\ No newline at end of file\n");
                        }
                    }
                }
                result.push(Hunk { patch });
            }
            old_line += a.lines().count();
            new_line += b.lines().count();
        }
        result
    }
}
pub async fn load(repo: &ReadonlyRepo, id: &str, path: &str) -> Result<Session> {
    validate_path(path)?;
    let id = CommitId::try_from_hex(id).context("Invalid commit ID")?;
    let commit = repo.store().get_commit_async(&id).await?;
    let path_ref = RepoPath::from_internal_string(path)?;
    let parent = commit.parent_tree(repo.index()).await?;
    let before = read_text(repo, path_ref, &parent.path_value(path_ref).await?).await?;
    let after = read_text(repo, path_ref, &commit.tree().path_value(path_ref).await?).await?;
    if let (Some(a), Some(b)) = (&before, &after) {
        ensure!(
            a.executable == b.executable,
            "Use whole-file actions for executable-mode changes"
        );
    }
    let session = Session {
        commit_id: id.hex(),
        change_id: commit.change_id().reverse_hex(),
        path: path.into(),
        before,
        after,
    };
    ensure!(
        !session.hunks().is_empty(),
        "No editable text hunks in this file"
    );
    Ok(session)
}
async fn read_text(
    repo: &ReadonlyRepo,
    path: &RepoPath,
    value: &MergedTreeValue,
) -> Result<Option<TextFile>> {
    match value.as_resolved() {
        Some(None) => Ok(None),
        Some(Some(TreeValue::File { id, executable, .. })) => {
            let mut bytes = Vec::new();
            repo.store()
                .read_file(path, id)
                .await?
                .take(256 * 1024 + 1)
                .read_to_end(&mut bytes)
                .await?;
            ensure!(
                bytes.len() <= 256 * 1024,
                "Chunk editing is limited to files up to 256 KiB"
            );
            ensure!(
                !bytes.contains(&0),
                "Binary files cannot be edited as text hunks"
            );
            Ok(Some(TextFile {
                text: String::from_utf8(bytes).context("Chunk editing requires UTF-8 text")?,
                executable: *executable,
            }))
        }
        _ => bail!(
            "Chunk editing supports regular text files; use whole-file actions for conflicts, symlinks, and submodules"
        ),
    }
}
fn selected_content(request: &Request) -> Result<Option<TextFile>> {
    let session = &request.session;
    let count = session.hunks().len();
    ensure!(!request.selected.is_empty(), "Select at least one hunk");
    ensure!(
        request.selected.iter().all(|i| *i < count),
        "Invalid hunk selection"
    );
    let old = session.before.as_ref().map_or("", |f| f.text.as_str());
    let new = session.after.as_ref().map_or("", |f| f.text.as_str());
    let diff = ContentDiff::by_line([old, new]);
    let mut index = 0;
    let mut text = String::new();
    for hunk in diff.hunks() {
        let side = if hunk.kind == DiffHunkKind::Matching {
            0
        } else {
            let selected = request.selected.contains(&index);
            index += 1;
            usize::from(match request.action {
                Action::Restore => !selected,
                Action::Split => selected,
            })
        };
        text.push_str(std::str::from_utf8(hunk.contents[side].as_ref())?);
    }
    let all = request.selected.len() == count;
    if all {
        return Ok(match request.action {
            Action::Restore => session.before.clone(),
            Action::Split => session.after.clone(),
        });
    }
    let mode = session
        .after
        .as_ref()
        .or(session.before.as_ref())
        .context("Missing file")?
        .executable;
    Ok(Some(TextFile {
        text,
        executable: mode,
    }))
}
fn validate_path(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && Path::new(path)
                .components()
                .all(|c| matches!(c, Component::Normal(name) if name != ".jj" && name != ".git")),
        "Invalid repository file path"
    );
    Ok(())
}
fn safe_path(root: &Path, relative: &str) -> Result<PathBuf> {
    validate_path(relative)?;
    let mut path = root.canonicalize()?;
    for part in Path::new(relative).components() {
        path.push(part);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => ensure!(
                !metadata.file_type().is_symlink(),
                "Refusing a symlink in the diff-editor path"
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(path)
}
fn disk_text(path: &Path) -> Result<Option<TextFile>> {
    match std::fs::read(path) {
        Ok(bytes) => {
            #[cfg(unix)]
            let executable = {
                use std::os::unix::fs::PermissionsExt;
                std::fs::metadata(path)?.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let executable = false;
            Ok(Some(TextFile {
                text: String::from_utf8(bytes)?,
                executable,
            }))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
/// Invoked by jj in temporary diff directories, never in the actual workspace.
pub fn edit(plan: &Path, left: &Path, right: &Path) -> Result<()> {
    let request: Request = serde_json::from_slice(&std::fs::read(plan)?)?;
    let old_path = safe_path(left, &request.session.path)?;
    let new_path = safe_path(right, &request.session.path)?;
    ensure!(
        disk_text(&old_path)? == request.session.before
            && disk_text(&new_path)? == request.session.after,
        "File changed since hunk selection; reopen the chunk editor and select again"
    );
    match selected_content(&request)? {
        Some(file) => {
            std::fs::create_dir_all(new_path.parent().context("Missing parent directory")?)?;
            std::fs::write(&new_path, file.text)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    &new_path,
                    std::fs::Permissions::from_mode(if file.executable { 0o755 } else { 0o644 }),
                )?;
            }
        }
        None => {
            if new_path.exists() {
                std::fs::remove_file(new_path)?;
            }
        }
    }
    Ok(())
}
pub fn apply(root: &Path, request: &Request) -> Result<String> {
    validate_path(&request.session.path)?;
    selected_content(request)?;
    let temp = tempfile::tempdir()?;
    let plan = temp.path().join("hunks.json");
    std::fs::write(&plan, serde_json::to_vec(request)?)?;
    let executable = std::env::current_exe()?;
    let program = executable
        .to_str()
        .context("Executable path is not UTF-8")?;
    let args = [
        "--chunk-tool",
        plan.to_str().context("Temporary path is not UTF-8")?,
        "$left",
        "$right",
    ];
    let mut command = Command::new("jj");
    command.current_dir(root).args([
        "--no-pager",
        "--color=never",
        "--config",
        &format!(
            "merge-tools.jjui-chunks.program={}",
            serde_json::to_string(program)?
        ),
        "--config",
        &format!(
            "merge-tools.jjui-chunks.edit-args={}",
            serde_json::to_string(&args)?
        ),
    ]);
    match request.action {
        Action::Restore => {
            command.args([
                "diffedit",
                "-r",
                &request.session.change_id,
                "--tool",
                "jjui-chunks",
            ]);
        }
        Action::Split => {
            command.args([
                "split",
                "-r",
                &request.session.change_id,
                "--tool",
                "jjui-chunks",
                &format!("--message={}", request.description),
            ]);
        }
    }
    command.args([
        "--",
        &format!(
            "root-file:{}",
            serde_json::to_string(&request.session.path)?
        ),
    ]);
    crate::commands::execute_process(command, "apply selected hunks")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(action: Action) -> Request {
        Request {
            session: Session {
                commit_id: "a".into(),
                change_id: "b".into(),
                path: "file.txt".into(),
                before: Some(TextFile {
                    text: "old\nkeep\nold2".into(),
                    executable: false,
                }),
                after: Some(TextFile {
                    text: "new\nkeep\nnew2".into(),
                    executable: false,
                }),
            },
            selected: [0].into(),
            action,
            description: "split".into(),
        }
    }
    #[test]
    fn selection_preserves_other_hunks_and_missing_newline() {
        let mut r = request(Action::Restore);
        assert_eq!(r.session.hunks().len(), 2);
        assert_eq!(
            selected_content(&r).unwrap().unwrap().text,
            "old\nkeep\nnew2"
        );
        r.action = Action::Split;
        assert_eq!(
            selected_content(&r).unwrap().unwrap().text,
            "new\nkeep\nold2"
        );
        r.selected.clear();
        assert!(selected_content(&r).is_err());
    }
    #[test]
    fn tool_rejects_stale_content_and_unsafe_paths() {
        let temp = tempfile::tempdir().unwrap();
        let left = temp.path().join("left");
        let right = temp.path().join("right");
        std::fs::create_dir(&left).unwrap();
        std::fs::create_dir(&right).unwrap();
        let r = request(Action::Restore);
        let plan = temp.path().join("plan.json");
        std::fs::write(&plan, serde_json::to_vec(&r).unwrap()).unwrap();
        std::fs::write(left.join("file.txt"), "old\nkeep\nold2").unwrap();
        std::fs::write(right.join("file.txt"), "fresh edits").unwrap();
        assert!(edit(&plan, &left, &right).is_err());
        assert_eq!(
            std::fs::read_to_string(right.join("file.txt")).unwrap(),
            "fresh edits"
        );
        assert!(validate_path("../escape").is_err());
        assert!(validate_path(".jj/config").is_err());
    }
}
