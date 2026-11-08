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
    GitOperations(String),
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
        Query::GitOperations(id) => {
            remotes = jj_lib::git::get_git_repo(repo.store())?
                .remote_names()
                .iter()
                .map(ToString::to_string)
                .collect();
            remotes.sort();
            for remote in &remotes {
                for kind in ["fetch", "push"] {
                    let spec =
                        crate::commands::action(kind, std::slice::from_ref(id), remote, &[])?;
                    rows.push(Row {
                        id: format!("{kind}:{remote}"),
                        label: format!("{kind} {remote}"),
                        operation: None,
                        bookmark: Some(BookmarkAction {
                            kind: kind.into(),
                            name: remote.clone(),
                            remote: Some(remote.clone()),
                            local: false,
                            args: spec.args,
                        }),
                    });
                }
            }
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
