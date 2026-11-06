use anyhow::{Context, Result};
use futures::{AsyncReadExt, StreamExt};
use jj_lib::{
    backend::{CommitId, MergedTreeValue, TreeValue},
    diff::{ContentDiff, DiffHunkKind},
    matchers::EverythingMatcher,
    object_id::ObjectId,
    repo::{ReadonlyRepo, Repo},
    repo_path::RepoPath,
    store::Store,
};
use serde::Serialize;

const MAX_FILE_BYTES: u64 = 256 * 1024;
const MAX_PATCH_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct FileDiff {
    pub path: String,
    pub status: String,
    pub patch: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Preview {
    pub commit_id: String,
    pub files: Vec<FileDiff>,
    pub truncated: bool,
}
impl Preview {
    pub fn patch(&self, selected: Option<usize>) -> String {
        let mut text = match selected {
            Some(index) => self
                .files
                .get(index)
                .map(|f| f.patch.clone())
                .unwrap_or_default(),
            None => self
                .files
                .iter()
                .map(|f| f.patch.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        };
        if text.is_empty() {
            text = "(no file changes)".into();
        }
        if self.truncated {
            text.push_str("\n[Preview limit reached; remaining files omitted]\n");
        }
        text
    }
}

pub async fn load(repo: &ReadonlyRepo, id: &str) -> Result<Preview> {
    let id = CommitId::try_from_hex(id).context("Invalid commit ID")?;
    let commit = repo.store().get_commit_async(&id).await?;
    let before = commit.parent_tree(repo).await?;
    let after = commit.tree();
    let mut entries = before.diff_stream(&after, &EverythingMatcher);
    let mut preview = Preview {
        commit_id: id.hex(),
        files: Vec::new(),
        truncated: false,
    };
    let mut total = 0;
    while let Some(entry) = entries.next().await {
        if preview.files.len() >= 200 || total >= MAX_PATCH_BYTES {
            preview.truncated = true;
            break;
        }
        let values = entry.values?;
        let path = entry.path.as_internal_file_string().to_owned();
        let status = if values.before.as_resolved() == Some(&None) {
            "A"
        } else if values.after.as_resolved() == Some(&None) {
            "D"
        } else if values.before.as_resolved().is_none() || values.after.as_resolved().is_none() {
            "C"
        } else {
            "M"
        }
        .to_owned();
        let old = content(repo.store(), &entry.path, &values.before).await?;
        let new = content(repo.store(), &entry.path, &values.after).await?;
        let metadata = format!(
            "--- {path} ({})\n+++ {path} ({})\n",
            kind(&values.before),
            kind(&values.after)
        );
        let patch = match (old, new) {
            (Content::Text(old), Content::Text(new)) => metadata + &text_diff(&old, &new),
            (a, b) => format!("{metadata}{} → {}\n", a.label(), b.label()),
        };
        total += patch.len();
        preview.files.push(FileDiff {
            path,
            status,
            patch,
        });
    }
    Ok(preview)
}

enum Content {
    Text(String),
    Unavailable(&'static str),
}
impl Content {
    fn label(&self) -> &str {
        match self {
            Self::Text(_) => "text",
            Self::Unavailable(reason) => reason,
        }
    }
}

fn kind(value: &MergedTreeValue) -> &'static str {
    match value.as_resolved() {
        None => "conflict",
        Some(None) => "absent",
        Some(Some(TreeValue::File {
            executable: true, ..
        })) => "executable",
        Some(Some(TreeValue::File { .. })) => "file",
        Some(Some(TreeValue::Symlink(_))) => "symlink",
        Some(Some(TreeValue::GitSubmodule(_))) => "submodule",
        Some(Some(TreeValue::Tree(_))) => "directory",
    }
}
async fn content(store: &Store, path: &RepoPath, value: &MergedTreeValue) -> Result<Content> {
    let Some(value) = value.as_resolved() else {
        return Ok(Content::Unavailable("unresolved conflict"));
    };
    let bytes = match value {
        None => return Ok(Content::Text(String::new())),
        Some(TreeValue::File { id, .. }) => {
            let mut bytes = Vec::new();
            store
                .read_file(path, id)
                .await?
                .take(MAX_FILE_BYTES + 1)
                .read_to_end(&mut bytes)
                .await?;
            if bytes.len() as u64 > MAX_FILE_BYTES {
                return Ok(Content::Unavailable("file exceeds 256 KiB"));
            }
            bytes
        }
        Some(TreeValue::Symlink(id)) => {
            return Ok(Content::Text(store.read_symlink(path, id).await?));
        }
        Some(TreeValue::GitSubmodule(id)) => {
            return Ok(Content::Text(format!("Subproject commit {}\n", id.hex())));
        }
        Some(TreeValue::Tree(_)) => return Ok(Content::Unavailable("directory")),
    };
    if bytes.contains(&0) {
        return Ok(Content::Unavailable("binary file"));
    }
    Ok(match String::from_utf8(bytes) {
        Ok(text) => Content::Text(text),
        Err(_) => Content::Unavailable("non-UTF-8 file"),
    })
}

fn text_diff(old: &str, new: &str) -> String {
    if old == new {
        return "(content unchanged; metadata changed)\n".into();
    }
    let mut output = format!(
        "@@ -{},{} +{},{} @@\n",
        usize::from(!old.is_empty()),
        old.lines().count(),
        usize::from(!new.is_empty()),
        new.lines().count()
    );
    let diff = ContentDiff::by_line([old, new]);
    for hunk in diff.hunks() {
        match hunk.kind {
            DiffHunkKind::Matching => append_lines(&mut output, ' ', hunk.contents[0].as_ref()),
            DiffHunkKind::Different => {
                append_lines(&mut output, '-', hunk.contents[0].as_ref());
                append_lines(&mut output, '+', hunk.contents[1].as_ref());
            }
        }
    }
    output
}
fn append_lines(output: &mut String, prefix: char, bytes: &[u8]) {
    let text = String::from_utf8_lossy(bytes);
    for line in text.split_inclusive('\n') {
        output.push(prefix);
        output.push_str(line);
        if !line.ends_with('\n') {
            output.push_str("\n\\ No newline at end of file\n");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn changes_and_missing_final_newline() {
        let patch = text_diff("same\nold\n", "same\nnew");
        assert!(patch.contains(" same\n-old\n+new\n\\ No newline"));
        assert!(text_diff("", "new\n").starts_with("@@ -0,0 +1,1 @@"));
    }
}
