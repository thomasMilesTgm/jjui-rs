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
    workspace::Workspace,
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
    pub empty: bool,
    pub conflict: bool,
    pub bookmarks: Vec<String>,
    pub working_copy: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub operation: String,
    pub revisions: Vec<Revision>,
}

pub struct Repository {
    workspace: Workspace,
    limit: usize,
    operation: Option<String>,
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
        let workspace = Workspace::load(
            &settings,
            root,
            &default_backend_factories(),
            &default_working_copy_factories(),
        )?;
        Ok(Self {
            workspace,
            limit,
            operation: None,
            repo: None,
            revset: "all()".into(),
        })
    }

    pub async fn snapshot(&mut self) -> Result<()> {
        crate::snapshot::snapshot(&mut self.workspace).await
    }

    pub async fn preview(&self, id: &str) -> Result<crate::preview::Preview> {
        crate::preview::load(self.repo.as_deref().context("Repository not loaded")?, id).await
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

    pub fn root(&self) -> PathBuf {
        self.workspace.workspace_root().to_owned()
    }
    pub fn repo_path(&self) -> PathBuf {
        self.workspace.repo_path().to_owned()
    }

    pub async fn refresh(&mut self, force: bool) -> Result<Option<Snapshot>> {
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
        if !force && self.operation.as_ref() == Some(&operation) {
            return Ok(None);
        }
        let op = loader.load_operation(&heads[0]).await?;
        let repo = loader.load_at(&op).await?;
        let wc = repo
            .view()
            .get_wc_commit_id(self.workspace.workspace_name());
        let revset = evaluate_revset(
            repo.as_ref(),
            self.workspace.settings(),
            &self.root(),
            self.workspace.workspace_name(),
            &self.revset,
        )?;
        let mut revisions = Vec::new();
        let mut stream = revset.stream().take(self.limit);
        while let Some(id) = stream.next().await {
            let id = id?;
            let commit = repo.store().get_commit_async(&id).await?;
            let bookmarks = repo
                .view()
                .local_bookmarks_for_commit(&id)
                .map(|(name, _)| name.as_str().to_owned())
                .collect();
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
                empty: commit.is_empty(repo.as_ref()).await?,
                conflict: commit.has_conflict(),
                bookmarks,
                working_copy: wc == Some(&id),
            });
        }
        self.repo = Some(repo.clone());
        self.operation = Some(operation.clone());
        Ok(Some(Snapshot {
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
        date_pattern_context: chrono::Local::now().into(),
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
