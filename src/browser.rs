use anyhow::{Context, Result};
use futures::StreamExt;
#[cfg(test)]
use jj_lib::object_id::ObjectId;
use jj_lib::{
    backend::CommitId,
    repo::{ReadonlyRepo, Repo},
};
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum Query {
    Operations,
    CommitOperations(String),
    #[cfg(test)]
    Bookmarks,
    BookmarkOperations(String),
    Files(String),
    /// Selected (or marked) revisions, as commit IDs in log order.
    GitOperations(Vec<String>),
}
#[derive(Clone, Debug, Serialize)]
pub struct Row {
    pub id: String,
    pub label: String,
    pub operation: Option<crate::operations::Entry>,
    pub bookmark: Option<BookmarkAction>,
}
#[derive(Clone, Debug, Serialize)]
pub struct BookmarkAction {
    pub kind: String,
    pub name: String,
    pub remote: Option<String>,
    pub local: bool,
    /// Local bookmark target, used to order the menu by graph distance.
    pub target: Option<String>,
    /// Explanation shown under the label; defaults to the jj arguments.
    pub description: Option<String>,
    /// jjui-style shortcut, active while a category filter is applied.
    pub key: Option<String>,
    pub args: Vec<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Listing {
    pub query: Query,
    pub rows: Vec<Row>,
    pub remotes: Vec<String>,
}

pub async fn load(repo: &ReadonlyRepo, query: Query) -> Result<Listing> {
    let mut rows = Vec::new();
    let mut remotes = Vec::new();
    match &query {
        Query::Operations | Query::CommitOperations(_) => {
            let commit = match &query {
                Query::CommitOperations(id) => Some(id.as_str()),
                _ => None,
            };
            rows = crate::operations::load(repo, commit).await?;
        }
        Query::GitOperations(ids) => {
            let result = git_items(repo, ids).await?;
            rows = result.0;
            remotes = result.1;
        }
        Query::BookmarkOperations(id) => {
            let result = crate::bookmarks::load(repo, id).await?;
            rows = result.0;
            remotes = result.1;
        }
        #[cfg(test)]
        Query::Bookmarks => {
            for (name, target) in repo.view().local_bookmarks() {
                rows.push(Row {
                    operation: None,
                    bookmark: None,
                    id: name.as_str().into(),
                    label: format!(
                        "{}  {}",
                        name.as_str(),
                        target
                            .present_adds()
                            .map(|id| format!("{:.12}", id.hex()))
                            .collect::<Vec<_>>()
                            .join(" ")
                    ),
                });
            }
            for (symbol, remote) in repo.view().all_remote_bookmarks() {
                rows.push(Row {
                    operation: None,
                    bookmark: None,
                    id: symbol.to_string(),
                    label: format!(
                        "{}  {}",
                        symbol,
                        if remote.is_tracked() {
                            "tracked"
                        } else {
                            "untracked"
                        }
                    ),
                });
            }
        }
        Query::Files(id) => {
            let id = CommitId::try_from_hex(id).context("Invalid commit ID")?;
            let commit = repo.store().get_commit_async(&id).await?;
            let mut entries = commit.tree().entries();
            while let Some((path, value)) = entries.next().await {
                value?;
                rows.push(Row {
                    operation: None,
                    bookmark: None,
                    id: path.as_internal_file_string().into(),
                    label: path.as_internal_file_string().into(),
                });
            }
        }
    }
    Ok(Listing {
        query,
        rows,
        remotes,
    })
}

/// A bookmark on a selected revision: name, whether jj lists a local entry,
/// and its remote refs as (remote, tracked).
type RevisionBookmark = (String, bool, Vec<(String, bool)>);
/// label, description, jj arguments, category and shortcut key.
type GitItem<'a> = (String, String, Vec<String>, &'a str, Option<&'a str>);

/// Mirrors jjui's Git menu: a fixed set of push/fetch commands per remote,
/// plus bookmark pushes for the bookmarks on the selected revisions.
async fn git_items(repo: &ReadonlyRepo, ids: &[String]) -> Result<(Vec<Row>, Vec<String>)> {
    let mut remotes: Vec<String> = jj_lib::git::get_git_repo(repo.store())?
        .remote_names()
        .iter()
        .map(ToString::to_string)
        .collect();
    remotes.sort();
    struct Revision {
        change: String,
        // Excludes conflicted bookmarks, like jjui.
        bookmarks: Vec<RevisionBookmark>,
    }
    let mut revisions = Vec::new();
    for id in ids {
        let commit_id = CommitId::try_from_hex(id).context("Invalid Git operation revision")?;
        let commit = repo.store().get_commit_async(&commit_id).await?;
        let len = repo
            .shortest_unique_change_id_prefix_len(commit.change_id())
            .await?;
        let change = commit.change_id().reverse_hex()[..len].to_owned();
        let view = repo.view();
        let mut names = std::collections::BTreeSet::new();
        for (name, target) in view.local_bookmarks() {
            if target.present_adds().any(|t| *t == commit_id) {
                names.insert(name.as_str().to_owned());
            }
        }
        for (symbol, remote) in view.all_remote_bookmarks() {
            if symbol.remote.as_str() != "git"
                && remote.target.present_adds().any(|t| *t == commit_id)
            {
                names.insert(symbol.name.as_str().to_owned());
            }
        }
        let mut bookmarks = Vec::new();
        for name in names {
            let local = view.get_local_bookmark(jj_lib::ref_name::RefName::new(&name));
            if !local.is_resolved() {
                continue;
            }
            let refs: Vec<(String, bool)> = view
                .all_remote_bookmarks()
                .filter(|(symbol, _)| {
                    symbol.name.as_str() == name && symbol.remote.as_str() != "git"
                })
                .map(|(symbol, remote)| (symbol.remote.as_str().to_owned(), remote.is_tracked()))
                .collect();
            // jj lists a local entry for present and deleted-but-tracked bookmarks.
            let has_local = local.is_present() || refs.iter().any(|(_, tracked)| *tracked);
            bookmarks.push((name, has_local, refs));
        }
        revisions.push(Revision { change, bookmarks });
    }
    let multiple = revisions.len() > 1;
    let mut rows = Vec::new();
    for remote in &remotes {
        let mut items: Vec<GitItem> = Vec::new();
        let push = |extra: &[&str]| -> Vec<String> {
            ["git", "push"]
                .iter()
                .chain(extra)
                .map(|s| s.to_string())
                .collect()
        };
        let mut selected = Vec::new();
        for revision in &revisions {
            for (name, has_local, refs) in &revision.bookmarks {
                let pushable = *has_local
                    && (refs.is_empty() || refs.iter().any(|(r, tracked)| r == remote && *tracked));
                if pushable && !selected.contains(name) {
                    selected.push(name.clone());
                }
                for (ref_remote, _) in refs {
                    items.push((
                        format!("git push --bookmark {name} --remote {ref_remote}"),
                        format!("Git push bookmark {name} to {ref_remote}"),
                        push(&[
                            "--bookmark",
                            &crate::bookmarks::exact(name),
                            "--remote",
                            ref_remote,
                        ]),
                        "push",
                        None,
                    ));
                }
            }
        }
        items.push((
            format!("git push --remote {remote}"),
            "Push tracking bookmarks in the current revset".into(),
            push(&["--remote", remote]),
            "push",
            Some("p"),
        ));
        items.push((
            format!("git push --all --deleted --remote {remote}"),
            "Push all bookmarks (including new and deleted bookmarks)".into(),
            push(&["--all", "--deleted", "--remote", remote]),
            "push",
            Some("a"),
        ));
        if multiple {
            let changes: Vec<String> = revisions
                .iter()
                .flat_map(|r| ["--change".to_owned(), r.change.clone()])
                .collect();
            let mut args = push(&["--remote", remote]);
            args.extend(changes.iter().cloned());
            items.push((
                format!("git push {}", changes.join(" ")),
                format!(
                    "Push selected changes ({})",
                    revisions
                        .iter()
                        .map(|r| r.change.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
                args,
                "push",
                Some("c"),
            ));
        }
        if !selected.is_empty() {
            let mut args = push(&["--remote", remote]);
            for name in &selected {
                args.extend(["--bookmark".into(), crate::bookmarks::exact(name)]);
            }
            items.push((
                format!(
                    "git push {} --remote {remote}",
                    selected
                        .iter()
                        .map(|name| format!("--bookmark {name}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
                format!(
                    "Push all bookmarks in selected changes ({})",
                    selected.join(", ")
                ),
                args,
                "push",
                Some("b"),
            ));
        }
        for revision in &revisions {
            items.push((
                format!("git push --change {} --remote {remote}", revision.change),
                format!("Push the current change ({})", revision.change),
                push(&["--change", &revision.change, "--remote", remote]),
                "push",
                (!multiple).then_some("c"),
            ));
        }
        let fetch = |extra: &[&str]| -> Vec<String> {
            ["git", "fetch"]
                .iter()
                .chain(extra)
                .map(|s| s.to_string())
                .collect()
        };
        items.extend([
            (
                format!("git push --deleted --remote {remote}"),
                "Push all deleted bookmarks".into(),
                push(&["--deleted", "--remote", remote]),
                "push",
                Some("d"),
            ),
            (
                format!("git push --tracked --remote {remote}"),
                "Push all tracked bookmarks".into(),
                push(&["--tracked", "--remote", remote]),
                "push",
                Some("t"),
            ),
            (
                format!("git fetch --remote {remote}"),
                "Fetch from remote".into(),
                fetch(&["--remote", remote]),
                "fetch",
                Some("f"),
            ),
            (
                format!("git fetch --tracked --remote {remote}"),
                "Fetch from remote".into(),
                fetch(&["--tracked", "--remote", remote]),
                "fetch",
                Some("t"),
            ),
            (
                "git fetch --all-remotes".into(),
                "Fetch from all remotes".into(),
                fetch(&["--all-remotes"]),
                "fetch",
                Some("a"),
            ),
        ]);
        for (index, (label, description, args, kind, key)) in items.into_iter().enumerate() {
            rows.push(Row {
                id: format!("{remote}:{index}:{label}"),
                label,
                operation: None,
                bookmark: Some(BookmarkAction {
                    kind: kind.into(),
                    name: remote.clone(),
                    remote: Some(remote.clone()),
                    local: false,
                    target: None,
                    description: Some(description),
                    key: key.map(str::to_owned),
                    args,
                }),
            });
        }
    }
    Ok((rows, remotes))
}
