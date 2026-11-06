use anyhow::{Context, Result};
use futures::StreamExt;
use jj_lib::{
    backend::CommitId,
    object_id::ObjectId,
    repo::{ReadonlyRepo, Repo},
};
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum Query {
    Operations,
    Bookmarks,
    Files(String),
}
#[derive(Clone, Debug, Serialize)]
pub struct Row {
    pub id: String,
    pub label: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Listing {
    pub query: Query,
    pub rows: Vec<Row>,
}

pub async fn load(repo: &ReadonlyRepo, query: Query) -> Result<Listing> {
    let mut rows = Vec::new();
    match &query {
        Query::Operations => {
            let mut stream = Box::pin(jj_lib::op_walk::walk_ancestors(&[repo.operation().clone()]));
            while let Some(op) = stream.next().await {
                let op = op?;
                let m = op.metadata();
                rows.push(Row {
                    id: op.id().hex(),
                    label: format!(
                        "{:.12}  {}@{}  {}",
                        op.id().hex(),
                        m.username,
                        m.hostname,
                        m.description
                    ),
                });
                if rows.len() >= 200 {
                    break;
                }
            }
        }
        Query::Bookmarks => {
            for (name, target) in repo.view().local_bookmarks() {
                rows.push(Row {
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
            for (path, value) in commit.tree().entries() {
                value?;
                rows.push(Row {
                    id: path.as_internal_file_string().into(),
                    label: path.as_internal_file_string().into(),
                });
            }
        }
    }
    Ok(Listing { query, rows })
}
