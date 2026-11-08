use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use jj_lib::{
    default_backend_factories::{default_backend_factories, default_working_copy_factories},
    fileset::FilesetAliasesMap,
    object_id::ObjectId,
    repo::{ReadonlyRepo, Repo},
    revset::{
        self, RevsetAliasesMap, RevsetDiagnostics, RevsetExtensions, RevsetParseContext,
        RevsetWorkspaceContext, SymbolResolver,
    },
    ui_path::RepoPathUiConverter,
    workspace::{DefaultWorkspaceLoaderFactory, Workspace, WorkspaceLoaderFactory},
};
use serde::Serialize;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Revision {
    pub commit_id: String,
    pub change_id: String,
    pub parents: Vec<String>,
    pub description: String,
    pub author: String,
    pub author_email: String,
    pub timestamp: String,
    pub change_prefix_len: usize,
    pub commit_prefix_len: usize,
    pub root: bool,
    pub immutable: bool,
    pub empty: bool,
    pub conflict: bool,
    pub bookmarks: Vec<String>,
    pub tags: Vec<String>,
    pub working_copy: bool,
    pub workspaces: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub workspace_stale: bool,
    pub operation: String,
    pub revisions: Vec<Revision>,
}

pub struct Repository {
    workspace: Workspace,
    limit: usize,
    operation: Option<String>,
    workspace_stale: bool,
    repo: Option<std::sync::Arc<ReadonlyRepo>>,
    revset: String,
}

impl Repository {
    pub fn open(path: &Path, limit: usize) -> Result<Self> {
        let path = path.canonicalize().context("Cannot open repository path")?;
        let root = path
            .ancestors()
            .find(|p| p.join(".jj").is_dir())
            .context("No Jujutsu workspace found; run inside a jj repository")?;
        let settings = crate::settings::load(root)?;
        let workspace = DefaultWorkspaceLoaderFactory.create(root)?.load(
            &settings,
            &default_backend_factories(),
            &default_working_copy_factories(),
        )?;
        Ok(Self {
            workspace,
            limit,
            operation: None,
            workspace_stale: false,
            repo: None,
            revset: "all()".into(),
        })
    }

    pub fn workspace_stale(&self) -> bool {
        self.workspace_stale
    }

    pub async fn snapshot(&mut self) -> Result<()> {
        crate::snapshot::snapshot(&mut self.workspace).await
    }

    pub async fn preview(&self, id: &str) -> Result<crate::preview::Preview> {
        crate::preview::load(self.repo.as_deref().context("Repository not loaded")?, id).await
    }

    pub async fn operation_diff(&self, id: &str) -> Result<String> {
        crate::operations::diff(self.repo.as_deref().context("Repository not loaded")?, id).await
    }
    pub async fn chunks(&self, id: &str, path: &str) -> Result<crate::chunks::Session> {
        crate::chunks::load(
            self.repo.as_deref().context("Repository not loaded")?,
            id,
            path,
        )
        .await
    }

    pub async fn set_revset(&mut self, expression: String) -> Result<Snapshot> {
        let previous = std::mem::replace(&mut self.revset, expression);
        match self.refresh(true).await {
            Ok(Some(snapshot)) => Ok(snapshot),
            Ok(None) => unreachable!("forced refresh"),
            Err(error) => {
                self.revset = previous;
                Err(error)
            }
        }
    }
    pub async fn listing(&self, query: crate::browser::Query) -> Result<crate::browser::Listing> {
        crate::browser::load(
            self.repo.as_deref().context("Repository not loaded")?,
            query,
        )
        .await
    }

    pub async fn workspaces(&mut self) -> Result<Vec<crate::workspaces::Entry>> {
        let repo = self.repo.as_ref().context("Repository not loaded")?;
        let mut entries = Vec::new();
        for (name, id) in repo.view().wc_commit_ids() {
            let path = if name.as_str() == self.workspace.workspace_name().as_str() {
                Some(self.root())
            } else {
                repo.loader().workspace_store().get_workspace_path(name)?
            };
            let commit = repo.store().get_commit_async(id).await?;
            let stale = if name.as_str() == self.workspace.workspace_name().as_str() {
                Some(self.workspace_stale)
            } else if let Some(path) = &path {
                match Self::open(path, self.limit) {
                    Ok(other) => crate::workspaces::is_stale(&other.workspace, repo)
                        .await
                        .ok(),
                    Err(_) => None,
                }
            } else {
                None
            };
            entries.push(crate::workspaces::Entry {
                stale,
                name: name.as_str().into(),
                current: name.as_str() == self.workspace.workspace_name().as_str(),
                available: path.as_ref().is_some_and(|p| p.join(".jj").is_dir()),
                path,
                commit: id.hex(),
                description: commit.description().lines().next().unwrap_or("").into(),
            });
        }
        Ok(entries)
    }

    pub fn reload_workspace(&mut self) -> Result<()> {
        let root = self.root();
        self.workspace = DefaultWorkspaceLoaderFactory.create(&root)?.load(
            &crate::settings::load(&root)?,
            &default_backend_factories(),
            &default_working_copy_factories(),
        )?;
        Ok(())
    }

    pub fn root(&self) -> PathBuf {
        self.workspace.workspace_root().to_owned()
    }
    pub fn repo_path(&self) -> PathBuf {
        self.workspace.repo_path().to_owned()
    }

    pub async fn refresh(&mut self, force: bool) -> Result<Option<Snapshot>> {
        self.reload_workspace()?;
        let loader = self.workspace.repo_loader();
        let heads = loader.op_heads_store().get_op_heads().await?;
        // load_at_head() can reconcile divergent operations and write a transaction.
        // A viewer must not silently do that.
        if heads.len() != 1 {
            bail!(
                "Repository has {} operation heads; run a jj command to reconcile them",
                heads.len()
            );
        }
        let operation = heads[0].hex();
        let unchanged = self.operation.as_ref() == Some(&operation);
        let repo = if unchanged && !force {
            self.repo.clone().context("Repository not loaded")?
        } else {
            let op = loader.load_operation(&heads[0]).await?;
            loader.load_at(&op).await?
        };
        let workspace_stale = crate::workspaces::is_stale(&self.workspace, &repo).await?;
        if !force && unchanged && self.workspace_stale == workspace_stale {
            return Ok(None);
        }
        self.workspace_stale = workspace_stale;
        let wc = repo
            .view()
            .get_wc_commit_id(self.workspace.workspace_name());
        let revset = evaluate_revset(
            repo.as_ref(),
            self.workspace.settings(),
            &self.root(),
            self.workspace.workspace_name(),
            // all() excludes rewritten commits, even when a remote still points
            // to one. Include those recorded remote states in the default graph.
            if self.revset.trim() == "all()" {
                "all() | remote_bookmarks() | tags()"
            } else {
                &self.revset
            },
        )?;
        let immutable = evaluate_revset(
            repo.as_ref(),
            self.workspace.settings(),
            &self.root(),
            self.workspace.workspace_name(),
            "immutable()",
        )?;
        let is_immutable = immutable.containing_fn();
        let mut revisions = Vec::new();
        let mut stream = revset.stream().take(self.limit);
        while let Some(id) = stream.next().await {
            let id = id?;
            let commit = repo.store().get_commit_async(&id).await?;
            let bookmarks = bookmark_labels(repo.view(), &id);
            revisions.push(Revision {
                commit_id: id.hex(),
                change_id: commit.change_id().reverse_hex(),
                parents: commit.parent_ids().iter().map(ObjectId::hex).collect(),
                description: commit.description().to_owned(),
                author: format!("{} <{}>", commit.author().name, commit.author().email),
                author_email: commit.author().email.clone(),
                timestamp: chrono::DateTime::from_timestamp_millis(
                    commit.committer().timestamp.timestamp.0,
                )
                .map(|time| {
                    time.with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string()
                })
                .unwrap_or_else(|| "(invalid timestamp)".into()),
                change_prefix_len: repo
                    .shortest_unique_change_id_prefix_len(commit.change_id())
                    .await?,
                commit_prefix_len: repo
                    .index()
                    .shortest_unique_commit_id_prefix_len(&id)
                    .await?,
                root: id == *repo.store().root_commit_id(),
                immutable: is_immutable(&id).await?,
                empty: commit.is_empty(repo.index()).await?,
                conflict: commit.has_conflict(),
                bookmarks,
                tags: repo
                    .view()
                    .local_tags()
                    .filter(|(_, target)| target.present_adds().any(|target| target == &id))
                    .map(|(name, _)| name.as_str().to_owned())
                    .collect(),
                working_copy: wc == Some(&id),
                workspaces: if repo.view().wc_commit_ids().len() > 1 {
                    repo.view()
                        .wc_commit_ids()
                        .iter()
                        .filter(|(_, commit)| *commit == &id)
                        .map(|(name, _)| format!("{}@", name.as_str()))
                        .collect()
                } else {
                    Vec::new()
                },
            });
        }
        self.repo = Some(repo.clone());
        self.operation = Some(operation.clone());
        Ok(Some(Snapshot {
            workspace_stale,
            operation,
            revisions,
        }))
    }
}

pub fn evaluate_revset(
    repo: &ReadonlyRepo,
    settings: &jj_lib::settings::UserSettings,
    root: &Path,
    workspace_name: &jj_lib::ref_name::WorkspaceName,
    expression: &str,
) -> Result<Box<dyn revset::Revset>> {
    let mut aliases = RevsetAliasesMap::default();
    for (name, body) in [
        (
            "trunk()",
            "latest(remote_bookmarks(exact:\"main\") | remote_bookmarks(exact:\"master\"))",
        ),
        (
            "builtin_immutable_heads()",
            "trunk() | tags() | remote_bookmarks() | root()",
        ),
        ("immutable_heads()", "builtin_immutable_heads()"),
        ("immutable()", "::immutable_heads()"),
        ("mutable()", "~immutable()"),
    ] {
        aliases.insert(name, body.to_owned(), None)?;
    }

    for name in settings.table_keys("revset-aliases") {
        let value: String = settings.get(["revset-aliases", name])?;
        aliases.insert(name, value, None)?;
    }
    let mut fileset_aliases = FilesetAliasesMap::default();
    for name in settings.table_keys("fileset-aliases") {
        let value: String = settings.get(["fileset-aliases", name])?;
        fileset_aliases.insert(name, value, None)?;
    }
    let extensions = RevsetExtensions::default();
    let path_converter = RepoPathUiConverter::Fs {
        cwd: root.to_owned(),
        base: root.to_owned(),
    };
    let context = RevsetParseContext {
        aliases_map: &aliases,
        local_variables: Default::default(),
        user_email: settings.user_email(),
        current_time: jiff::Zoned::now(),
        default_ignored_remote: Some(jj_lib::git::REMOTE_NAME_FOR_LOCAL_GIT_REPO),
        fileset_aliases_map: &fileset_aliases,
        extensions: &extensions,
        workspace: Some(RevsetWorkspaceContext {
            path_converter: &path_converter,
            workspace_name,
        }),
    };
    let parsed = revset::parse(&mut RevsetDiagnostics::new(), expression, &context)?;
    let resolver = SymbolResolver::new(repo, extensions.symbol_resolvers());
    let resolved = parsed.resolve_user_expression(repo, &resolver)?;
    Ok(resolved.evaluate()?)
}

// Match jj's decoration rules: tracked remotes are implicit only when equal.
fn bookmark_labels(view: &jj_lib::view::View, id: &jj_lib::backend::CommitId) -> Vec<String> {
    let mut labels = Vec::new();
    for (name, target) in view.local_bookmarks_for_commit(id) {
        let differs = view.all_remote_bookmarks().any(|(symbol, remote)| {
            symbol.name == name
                && symbol.remote.as_str() != "git"
                && remote.is_tracked()
                && remote.target != *target
        });
        labels.push(format!(
            "{}{}",
            name.as_str(),
            if differs { "*" } else { "" }
        ));
    }
    for (symbol, remote) in view.all_remote_bookmarks() {
        if symbol.remote.as_str() == "git"
            || !remote.target.present_adds().any(|target| target == id)
        {
            continue;
        }
        let local = view.get_local_bookmark(symbol.name);
        if !remote.is_tracked() || local != &remote.target {
            labels.push(symbol.to_string());
        }
    }
    labels
}
