//! Workspace metadata is read natively; mutations go through the explicit command runner.
use serde::Serialize;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub name: String,
    pub path: Option<PathBuf>,
    pub current: bool,
    pub available: bool,
    pub commit: String,
    pub description: String,
}

#[cfg(test)]
mod tests {
    use crate::{
        commands::{CommandSpec, execute_process},
        repository::Repository,
    };

    #[test]
    fn native_listing_tracks_paths_renames_missing_directories_and_forgetting() {
        let temp = tempfile::tempdir().unwrap();
        let execute = |root: &std::path::Path, spec: &CommandSpec| {
            let mut process = spec.process(root)?;
            process
                .env("XDG_CONFIG_HOME", temp.path().join("config"))
                .env("JJ_CONFIG", "");
            execute_process(process, &spec.display())
        };
        let main = temp.path().join("main");
        std::fs::create_dir(&main).unwrap();
        execute(&main, &CommandSpec::new(["git", "init", "--colocate"])).unwrap();
        std::fs::write(
            main.join(".jj/repo/config.toml"),
            "[user]\nname='Test'\nemail='test@example.org'\n",
        )
        .unwrap();
        let other = temp.path().join("other workspace");
        execute(
            &main,
            &CommandSpec::new([
                "workspace",
                "add",
                "--name=-literal",
                "--",
                other.to_str().unwrap(),
            ]),
        )
        .unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut repo = Repository::open(&main, 100).unwrap();
        let before = runtime.block_on(repo.refresh(true)).unwrap().unwrap();
        assert!(
            before
                .revisions
                .iter()
                .any(|r| r.workspaces.contains(&"-literal@".into()))
        );
        let entries = runtime.block_on(repo.workspaces()).unwrap();
        let entry = entries.iter().find(|e| e.name == "-literal").unwrap();
        assert_eq!(entry.path, Some(other.clone()));
        assert!(entry.available && !entry.current);
        assert!(entries.iter().any(|e| e.name == "default" && e.current));
        assert_eq!(
            runtime
                .block_on(repo.refresh(true))
                .unwrap()
                .unwrap()
                .operation,
            before.operation
        );

        let mut other_repo = Repository::open(&other, 100).unwrap();
        execute(
            &other,
            &CommandSpec::new(["workspace", "rename", "--", "renamed"]),
        )
        .unwrap();
        other_repo.reload_workspace().unwrap();
        runtime.block_on(other_repo.refresh(true)).unwrap();
        assert!(
            runtime
                .block_on(other_repo.workspaces())
                .unwrap()
                .iter()
                .any(|e| e.name == "renamed" && e.current)
        );
        let parked = temp.path().join("parked");
        std::fs::rename(&other, &parked).unwrap();
        runtime.block_on(repo.refresh(true)).unwrap();
        assert!(
            runtime
                .block_on(repo.workspaces())
                .unwrap()
                .iter()
                .any(|e| e.name == "renamed" && !e.available)
        );
        execute(
            &main,
            &CommandSpec::new(["workspace", "forget", "--", "renamed"]),
        )
        .unwrap();
        runtime.block_on(repo.refresh(true)).unwrap();
        assert_eq!(runtime.block_on(repo.workspaces()).unwrap().len(), 1);
        assert!(parked.join(".jj").exists());
    }
}
