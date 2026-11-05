use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use jj_lib::{
    config::{ConfigLayer, ConfigSource, StackedConfig},
    default_backend_factories::{default_backend_factories, default_working_copy_factories},
    object_id::ObjectId,
    repo::Repo,
    revset::ResolvedRevsetExpression,
    settings::UserSettings,
    workspace::Workspace,
};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct Revision {
    pub commit_id: String,
    pub change_id: String,
    pub parents: Vec<String>,
    pub description: String,
    pub author: String,
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
}

impl Repository {
    pub fn open(path: &Path, limit: usize) -> Result<Self> {
        let path = path.canonicalize().context("Cannot open repository path")?;
        let root = path
            .ancestors()
            .find(|p| p.join(".jj").is_dir())
            .context("No Jujutsu workspace found; run inside a jj repository")?;
        let mut config = StackedConfig::with_defaults();
        // No transactions or signing: these values only satisfy library settings.
        config.add_layer(ConfigLayer::parse(
            ConfigSource::EnvBase,
            r#"
            [user]
            name = "jjui-rs"
            email = ""
            [operation]
            hostname = "localhost"
            username = "jjui-rs"
        "#,
        )?);
        let settings = UserSettings::from_config_and_home_dir(config, None)?;
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
        })
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
        let revset = ResolvedRevsetExpression::all()
            .within_visibility(repo.as_ref())
            .evaluate(repo.as_ref())?;
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
                bookmarks,
                working_copy: wc == Some(&id),
            });
        }
        self.operation = Some(operation.clone());
        Ok(Some(Snapshot {
            operation,
            revisions,
        }))
    }
}
