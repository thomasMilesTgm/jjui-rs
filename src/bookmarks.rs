//! Native bookmark operation discovery. Only applying a menu item invokes jj.
use crate::browser::{BookmarkAction, Row};
use anyhow::{Context, Result};
use jj_lib::{
    backend::CommitId,
    repo::{ReadonlyRepo, Repo},
};

pub async fn load(repo: &ReadonlyRepo, id: &str) -> Result<(Vec<Row>, Vec<String>)> {
    let destination = CommitId::try_from_hex(id).context("Invalid bookmark destination")?;
    let commit = repo.store().get_commit_async(&destination).await?;
    let change = commit.change_id().reverse_hex();
    let mut remotes: Vec<String> = jj_lib::git::get_git_repo(repo.store())
        .map(|git| git.remote_names().iter().map(ToString::to_string).collect())
        .unwrap_or_default();
    remotes.sort();
    let default_remote = remotes.first().cloned().unwrap_or_else(|| "origin".into());
    let mut rows = Vec::new();
    let mut names = std::collections::BTreeSet::new();
    for (name, target) in repo.view().local_bookmarks() {
        let name = name.as_str();
        names.insert(name.to_owned());
        if !target.is_present() {
            continue;
        }
        if target.as_normal() != Some(&destination) {
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
            let qualifier = if backwards { " backwards" } else { "" };
            if related {
                rows.push(row(
                    "move",
                    name,
                    None,
                    true,
                    format!("move '{name}'{qualifier} to {:.8}", change),
                    args,
                ));
            }
        }
        rows.push(row(
            "delete",
            name,
            None,
            true,
            format!("delete '{name}'"),
            vec!["bookmark".into(), "delete".into(), "--".into(), exact(name)],
        ));
        let tracked = repo.view().all_remote_bookmarks().any(|(symbol, remote)| {
            symbol.name.as_str() == name && symbol.remote.as_str() != "git" && remote.is_tracked()
        });
        if !tracked {
            rows.push(row(
                "track",
                name,
                Some(default_remote.clone()),
                true,
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
            format!("{kind} '{symbol}'"),
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
        let priority = ["move", "delete", "track", "untrack", "forget"]
            .iter()
            .position(|kind| *kind == action.kind)
            .unwrap();
        (priority, action.name.clone(), action.remote.clone())
    });
    remotes.sort();
    remotes.dedup();
    remotes.insert(0, "local".into());
    Ok((rows, remotes))
}

fn row(
    kind: &str,
    name: &str,
    remote: Option<String>,
    local: bool,
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
            args,
        }),
    }
}

fn exact(value: &str) -> String {
    format!("exact:{}", serde_json::to_string(value).unwrap())
}
