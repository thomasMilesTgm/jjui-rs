//! Native bookmark operation discovery. Only applying a menu item invokes jj.
use crate::browser::{BookmarkAction, Row};
use anyhow::{Context, Result};
use jj_lib::{
    backend::CommitId,
    object_id::ObjectId,
    repo::{ReadonlyRepo, Repo},
};
use std::collections::BTreeSet;

/// Mirrors jjui's bookmark menu: move (bookmarks before or after the
/// revision), delete, track, untrack and forget items. Rows are ordered by
/// kind here; the UI then orders each kind by graph distance like jjui.
pub async fn load(repo: &ReadonlyRepo, id: &str) -> Result<(Vec<Row>, Vec<String>)> {
    let destination = CommitId::try_from_hex(id).context("Invalid bookmark destination")?;
    let commit = repo.store().get_commit_async(&destination).await?;
    let change = commit.change_id().reverse_hex();
    let mut remotes: Vec<String> = jj_lib::git::get_git_repo(repo.store())
        .map(|git| git.remote_names().iter().map(ToString::to_string).collect())
        .unwrap_or_default();
    remotes.sort();
    // jjui tracks local bookmarks against the first remote after "local".
    let default_remote = remotes.first().cloned().unwrap_or_else(|| "origin".into());
    let mut rows = Vec::new();
    let mut names = BTreeSet::new();
    for (name, target) in repo.view().local_bookmarks() {
        let name = name.as_str();
        names.insert(name.to_owned());
        if !target.is_present() {
            continue;
        }
        let local_target = target.as_normal().map(|id| id.hex());
        let conflict = !target.is_resolved();
        if conflict || target.as_normal() != Some(&destination) {
            let mut backwards = false;
            let mut related = false;
            for source in target.present_adds() {
                let ancestor = repo.index().is_ancestor(source, &destination).await?;
                related |= ancestor || repo.index().is_ancestor(&destination, source).await?;
                if !ancestor {
                    backwards = true;
                }
            }
            let mut args = vec!["bookmark".into(), "move".into(), "--to".into(), id.into()];
            if backwards {
                args.push("--allow-backwards".into());
            }
            args.extend(["--".into(), exact(name)]);
            let label = if backwards && !conflict {
                format!("move '{name}' backwards to {change:.8}")
            } else if conflict {
                format!("move conflicted '{name}' to {change:.8}")
            } else {
                format!("move '{name}' to {change:.8}")
            };
            if related {
                rows.push(row(
                    "move",
                    name,
                    None,
                    true,
                    local_target.clone(),
                    label,
                    args,
                ));
            }
        }
        rows.push(row(
            "delete",
            name,
            None,
            true,
            local_target.clone(),
            format!("delete '{name}'"),
            vec!["bookmark".into(), "delete".into(), "--".into(), exact(name)],
        ));
        // Like jjui, only offer tracking a local bookmark that has no remote refs.
        let has_remote = repo
            .view()
            .all_remote_bookmarks()
            .any(|(symbol, _)| symbol.name.as_str() == name && symbol.remote.as_str() != "git");
        if !has_remote {
            rows.push(row(
                "track",
                name,
                Some(default_remote.clone()),
                true,
                local_target,
                format!("track '{name}'"),
                vec![
                    "bookmark".into(),
                    "track".into(),
                    "--remote".into(),
                    exact(&default_remote),
                    "--".into(),
                    exact(name),
                ],
            ));
        }
    }
    for (symbol, remote) in repo.view().all_remote_bookmarks() {
        if symbol.remote.as_str() == "git" {
            continue;
        }
        let name = symbol.name.as_str();
        names.insert(name.to_owned());
        let remote_name = symbol.remote.as_str().to_owned();
        if !remotes.contains(&remote_name) {
            remotes.push(remote_name.clone());
        }
        let kind = if remote.is_tracked() {
            "untrack"
        } else {
            "track"
        };
        let remote_pattern = exact(&remote_name);
        rows.push(row(
            kind,
            name,
            Some(remote_name),
            remote.is_tracked(),
            local_target(repo, name),
            format!("{kind} '{name}@{}'", symbol.remote.as_str()),
            vec![
                "bookmark".into(),
                kind.into(),
                "--remote".into(),
                remote_pattern,
                "--".into(),
                exact(name),
            ],
        ));
    }
    for name in names {
        rows.push(row(
            "forget",
            &name,
            None,
            true,
            local_target(repo, &name),
            format!("forget '{name}'"),
            vec![
                "bookmark".into(),
                "forget".into(),
                "--".into(),
                exact(&name),
            ],
        ));
    }
    rows.sort_by_key(|row| {
        let action = row.bookmark.as_ref().unwrap();
        (kind_priority(&action.kind), row.label.clone())
    });
    remotes.insert(0, "local".into());
    Ok((rows, remotes))
}

/// jjui's action order: move, delete, track, untrack, forget.
pub fn kind_priority(kind: &str) -> usize {
    ["move", "delete", "track", "untrack", "forget"]
        .iter()
        .position(|k| *k == kind)
        .unwrap_or(usize::MAX)
}

fn local_target(repo: &ReadonlyRepo, name: &str) -> Option<String> {
    repo.view()
        .get_local_bookmark(jj_lib::ref_name::RefName::new(name))
        .as_normal()
        .map(|id| id.hex())
}

fn row(
    kind: &str,
    name: &str,
    remote: Option<String>,
    local: bool,
    target: Option<String>,
    label: String,
    args: Vec<String>,
) -> Row {
    Row {
        id: format!("{kind}:{name}:{local}:{}", remote.as_deref().unwrap_or("")),
        label,
        operation: None,
        bookmark: Some(BookmarkAction {
            kind: kind.into(),
            name: name.into(),
            remote,
            local,
            target,
            description: None,
            // jjui's shortcut: repeating `m` moves main/master.
            key: (kind == "move" && matches!(name, "main" | "master")).then(|| "m".into()),
            args,
        }),
    }
}

pub fn exact(value: &str) -> String {
    format!("exact:{}", serde_json::to_string(value).unwrap())
}
