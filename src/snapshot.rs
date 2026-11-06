//! Opt-in snapshotting uses the workspace lock and checks freshness before any
//! rewrite. A stale workspace is reported, never silently checked out.
use anyhow::{Context, Result, bail};
use jj_lib::{
    fileset::{self, FilesetAliasesMap, FilesetDiagnostics, FilesetParseContext},
    gitignore::GitIgnoreFile,
    matchers::NothingMatcher,
    repo::Repo,
    repo_path::RepoPath,
    settings::HumanByteSize,
    ui_path::RepoPathUiConverter,
    working_copy::{SnapshotOptions, WorkingCopyFreshness},
    workspace::Workspace,
};

pub async fn snapshot(workspace: &mut Workspace) -> Result<()> {
    let loader = workspace.repo_loader().clone();
    let root = workspace.workspace_root().to_owned();
    let name = workspace.workspace_name().to_owned();
    let settings = workspace.settings().clone();
    let converter = RepoPathUiConverter::Fs {
        cwd: root.clone(),
        base: root.clone(),
    };
    let mut aliases = FilesetAliasesMap::default();
    for name in settings.table_keys("fileset-aliases") {
        let value: String = settings.get(["fileset-aliases", name])?;
        aliases.insert(name, value, None)?;
    }
    let expression: String = settings.get("snapshot.auto-track")?;
    let matcher = fileset::parse(
        &mut FilesetDiagnostics::new(),
        &expression,
        &FilesetParseContext {
            aliases_map: &aliases,
            path_converter: &converter,
        },
    )?
    .to_matcher();
    let max_size = settings
        .config()
        .get_value_with("snapshot.max-new-file-size", HumanByteSize::try_from)?
        .0;
    let mut lock = workspace.start_working_copy_mutation().await?;
    let heads = loader.op_heads_store().get_op_heads().await?;
    if heads.len() != 1 {
        bail!("Cannot snapshot divergent operation heads; reconcile with jj first");
    }
    let op = loader.load_operation(&heads[0]).await?;
    let repo = loader.load_at(&op).await?;
    let wc = repo
        .view()
        .get_wc_commit_id(&name)
        .context("Workspace has no working-copy commit")?;
    let commit = repo.store().get_commit_async(wc).await?;
    if !matches!(
        WorkingCopyFreshness::check_stale(lock.locked_wc(), &commit, &repo).await?,
        WorkingCopyFreshness::Fresh
    ) {
        bail!(
            "Workspace is stale or concurrently updated; run jj workspace update-stale before snapshotting"
        );
    }
    let mut ignores = GitIgnoreFile::empty();
    if let Some(backend) = repo
        .store()
        .backend_impl::<jj_lib::git_backend::GitBackend>()
    {
        let git = backend.git_repo();
        ignores =
            ignores.chain_with_file(RepoPath::root(), git.common_dir().join("info/exclude"))?;
        if let Some(path) = git.config_snapshot().string("core.excludesFile") {
            let path = String::from_utf8_lossy(path.as_ref()).into_owned();
            let path = if let Some(suffix) = path.strip_prefix("~/") {
                std::env::var_os("HOME")
                    .map(std::path::PathBuf::from)
                    .map(|p| p.join(suffix))
                    .unwrap_or_else(|| path.clone().into())
            } else {
                path.into()
            };
            ignores = ignores.chain_with_file(RepoPath::root(), path)?;
        }
    }
    let options = SnapshotOptions {
        base_ignores: ignores,
        progress: None,
        start_tracking_matcher: matcher.as_ref(),
        force_tracking_matcher: &NothingMatcher,
        max_new_file_size: max_size,
    };
    let (tree, stats) = lock.locked_wc().snapshot(&options).await?;
    if !stats.invalid_utf8_paths.is_empty() {
        bail!("Snapshot skipped invalid UTF-8 paths");
    }
    if tree.tree_ids_and_labels() == commit.tree().tree_ids_and_labels() {
        lock.finish(repo.op_id().clone()).await?;
        return Ok(());
    }
    let immutable =
        crate::repository::evaluate_revset(&repo, &settings, &root, &name, "immutable()")?;
    if immutable.containing_fn()(commit.id()).await? {
        bail!(
            "Refusing to automatically snapshot an immutable working-copy commit; create a new change with jj new"
        );
    }
    let mut transaction = repo.start_transaction();
    transaction.set_is_snapshot(true);
    transaction.set_workspace_name(&name);
    let rewritten = transaction
        .repo_mut()
        .rewrite_commit(&commit)
        .set_tree(tree)
        .write()
        .await?;
    transaction
        .repo_mut()
        .set_wc_commit(name, rewritten.id().clone())?;
    transaction.repo_mut().rebase_descendants().await?;
    let repo = transaction
        .commit("snapshot working copy from jjui-rs")
        .await?;
    lock.finish(repo.op_id().clone()).await?;
    Ok(())
}
