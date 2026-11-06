//! Read-only operation history, optionally filtered by a change's state transitions.
use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::{Context, Result};
use futures::StreamExt;
use jj_lib::{
    backend::{ChangeId, CommitId},
    object_id::ObjectId,
    operation::Operation,
    repo::{ReadonlyRepo, Repo},
};
use serde::Serialize;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Entry {
    pub parents: Vec<String>,
    pub current: bool,
    pub root: bool,
    pub snapshot: bool,
    pub prefix_len: usize,
    pub user: String,
    pub workspace: Option<String>,
    pub end_millis: i64,
    pub duration_millis: i64,
    pub description: String,
    pub attributes: Vec<String>,
}

// Compare only the selected change, including rewrites and refs pointing to it.
// Loading at an operation never reconciles heads or snapshots the working copy.
async fn change_state(
    repo: &ReadonlyRepo,
    op: &Operation,
    change: &ChangeId,
) -> Result<BTreeSet<String>> {
    let historical = repo.reload_at(op).await?;
    let mut state = BTreeSet::new();
    let targets = historical.resolve_change_id(change).await?;
    let visible: HashSet<_> = targets
        .as_ref()
        .into_iter()
        .flat_map(|targets| targets.visible_with_offsets().map(|(_, id)| id.clone()))
        .collect();
    for id in &visible {
        state.insert(format!("commit {}", id.hex()));
    }
    let view = historical.view();
    for (name, target) in view.local_bookmarks() {
        if target.present_adds().any(|id| visible.contains(id)) {
            state.insert(format!("bookmark {} {target:?}", name.as_str()));
        }
    }
    for (name, target) in view.all_remote_bookmarks() {
        if target.target.present_adds().any(|id| visible.contains(id)) {
            state.insert(format!("remote {name} {target:?}"));
        }
    }
    for (name, target) in view.local_tags() {
        if target.present_adds().any(|id| visible.contains(id)) {
            state.insert(format!("tag {} {target:?}", name.as_str()));
        }
    }
    for (name, target) in view.all_remote_tags() {
        if target.target.present_adds().any(|id| visible.contains(id)) {
            state.insert(format!("remote tag {name} {target:?}"));
        }
    }
    for (name, id) in view.wc_commit_ids() {
        if visible.contains(id) {
            state.insert(format!("workspace {} {}", name.as_str(), id.hex()));
        }
    }
    Ok(state)
}

pub async fn load(repo: &ReadonlyRepo, commit: Option<&str>) -> Result<Vec<crate::browser::Row>> {
    let change = if let Some(id) = commit {
        let id = CommitId::try_from_hex(id).context("Invalid commit ID")?;
        Some(
            repo.store()
                .get_commit_async(&id)
                .await?
                .change_id()
                .clone(),
        )
    } else {
        None
    };
    let mut stream = Box::pin(jj_lib::op_walk::walk_ancestors(&[repo.operation().clone()]));
    let mut states = HashMap::new();
    let mut ancestry = HashMap::new();
    let mut rows = Vec::new();
    while let Some(op) = stream.next().await {
        let op = op?;
        let id = op.id().hex();
        let parents: Vec<_> = op.parent_ids().iter().map(ObjectId::hex).collect();
        ancestry.insert(id.clone(), parents.clone());
        if let Some(change) = &change {
            let state = match states.remove(&id) {
                Some(state) => state,
                None => change_state(repo, &op, change).await?,
            };
            let mut related = parents.is_empty() && !state.is_empty();
            for parent in op.parents().await? {
                let parent_id = parent.id().hex();
                if !states.contains_key(&parent_id) {
                    states.insert(
                        parent_id.clone(),
                        change_state(repo, &parent, change).await?,
                    );
                }
                related |= states[&parent_id] != state;
            }
            if !related {
                continue;
            }
        }
        let m = op.metadata();
        rows.push(crate::browser::Row {
            id,
            label: m.description.clone(),
            operation: Some(Entry {
                parents,
                current: op.id() == repo.operation().id(),
                root: op.parent_ids().is_empty(),
                snapshot: m.is_snapshot,
                prefix_len: unique_prefix_len(repo, op.id()).await?,
                user: format!("{}@{}", m.username, m.hostname),
                workspace: m
                    .workspace_name
                    .as_ref()
                    .map(|name| name.as_str().to_owned()),
                end_millis: m.time.end.timestamp.0,
                duration_millis: m
                    .time
                    .end
                    .timestamp
                    .0
                    .saturating_sub(m.time.start.timestamp.0),
                description: m.description.clone(),
                attributes: m
                    .attributes
                    .iter()
                    .map(|(key, value)| format!("{key}: {value}"))
                    .collect(),
            }),
        });
        if rows.len() == 200 {
            break;
        }
    }
    // Contract omitted operations, retaining the real DAG rather than pretending
    // every related operation is the parent of the preceding one.
    contract_parents(&mut rows, &ancestry);
    Ok(rows)
}

async fn unique_prefix_len(
    repo: &ReadonlyRepo,
    id: &jj_lib::op_store::OperationId,
) -> Result<usize> {
    use jj_lib::object_id::{HexPrefix, PrefixResolution};
    let hex = id.hex();
    for len in 1..=hex.len() {
        let prefix = HexPrefix::try_from_hex(&hex[..len]).context("Invalid operation prefix")?;
        if matches!(
            repo.op_store().resolve_operation_id_prefix(&prefix).await?,
            PrefixResolution::SingleMatch(_)
        ) {
            return Ok(len);
        }
    }
    Ok(hex.len())
}

/// Compare the recorded working-copy trees across the actual operation edge.
/// Filtered graph parents are deliberately not used: they can skip operations.
pub async fn diff(repo: &ReadonlyRepo, id: &str) -> Result<String> {
    let id = jj_lib::op_store::OperationId::try_from_hex(id).context("Invalid operation ID")?;
    let data = repo.op_store().read_operation(&id).await?;
    let op = Operation::new(repo.op_store().clone(), id, data);
    anyhow::ensure!(
        op.metadata().is_snapshot,
        "Diff is available for snapshot working-copy operations"
    );
    let parents = op.parents().await?;
    anyhow::ensure!(
        parents.len() == 1,
        "Snapshot diff requires exactly one parent operation"
    );
    let before = parents[0].view().await?;
    let after = op.view().await?;
    let mut text = format!(
        "Operation {}\nParent {}\n{}\n\n",
        op.id().hex(),
        parents[0].id().hex(),
        op.metadata().description
    );
    let mut changes = false;
    for (workspace, new_id) in after.wc_commit_ids() {
        let Some(old_id) = before.wc_commit_ids().get(workspace) else {
            continue;
        };
        if old_id == new_id {
            continue;
        }
        let preview = crate::preview::between_commits(repo, old_id, new_id).await?;
        if preview.files.is_empty() {
            continue;
        }
        changes = true;
        text.push_str(&format!(
            "Workspace {}\nBefore {}\nAfter {}\n\n{}\n",
            workspace.as_str(),
            old_id.hex(),
            new_id.hex(),
            preview.patch(None)
        ));
        if text.len() >= 2 * 1024 * 1024 {
            text.push_str("\n[Operation diff limit reached; remaining workspaces omitted]\n");
            break;
        }
    }
    if !changes {
        text.push_str("(no file changes)");
    }
    Ok(text)
}

fn contract_parents(rows: &mut [crate::browser::Row], ancestry: &HashMap<String, Vec<String>>) {
    let included: HashSet<_> = rows.iter().map(|row| row.id.clone()).collect();
    for row in rows {
        let entry = row.operation.as_mut().unwrap();
        let mut pending = entry.parents.clone();
        let mut seen = HashSet::new();
        entry.parents.clear();
        while let Some(id) = pending.pop() {
            if !seen.insert(id.clone()) {
                continue;
            }
            if included.contains(&id) || !ancestry.contains_key(&id) {
                entry.parents.push(id);
            } else {
                pending.extend(ancestry[&id].iter().rev().cloned());
            }
        }
        entry.parents.sort();
    }
}

pub fn graph(rows: &[crate::browser::Row]) -> Vec<crate::graph::GraphRow> {
    let mut graph = crate::graph::render(
        &rows
            .iter()
            .filter_map(|row| {
                row.operation
                    .as_ref()
                    .map(|op| crate::repository::Revision {
                        commit_id: row.id.clone(),
                        parents: op.parents.clone(),
                        working_copy: op.current,
                        ..Default::default()
                    })
            })
            .collect::<Vec<_>>(),
    );
    for row in &mut graph {
        row.edge = row
            .edge
            .replace('┌', "╭")
            .replace('┐', "╮")
            .replace('└', "╰")
            .replace('┘', "╯");
    }
    graph
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filtered_graph_contracts_skipped_operations_and_keeps_merges() {
        let mut rows: Vec<_> = [
            ("merge", vec!["skip-a", "b"]),
            ("a", vec!["root"]),
            ("b", vec!["skip-b"]),
            ("root", vec![]),
        ]
        .into_iter()
        .map(|(id, parents)| crate::browser::Row {
            id: id.into(),
            label: id.into(),
            operation: Some(Entry {
                parents: parents.into_iter().map(str::to_owned).collect(),
                current: id == "merge",
                ..Default::default()
            }),
        })
        .collect();
        let ancestry = HashMap::from([
            ("skip-a".into(), vec!["a".into()]),
            ("skip-b".into(), vec!["root".into()]),
        ]);
        contract_parents(&mut rows, &ancestry);
        assert_eq!(rows[0].operation.as_ref().unwrap().parents, ["a", "b"]);
        assert_eq!(rows[2].operation.as_ref().unwrap().parents, ["root"]);
        let graph = graph(&rows);
        assert_eq!(graph[0].node.trim_end(), "@");
        assert_eq!(graph[0].edge, "├─╮ ");
        assert_eq!(graph[2].edge, "├─╯ ");
        assert!(graph[3].boundary.is_none());
    }
}
