//! These tests use jj only to prepare/mutate fixtures. The viewer gets an empty PATH.
use serde_json::Value;
use std::{
    io::{BufRead, BufReader},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};
use tempfile::TempDir;

fn jj(root: &Path, args: &[&str]) {
    let output = Command::new("jj")
        .args([
            "--config",
            "user.name=Test",
            "--config",
            "user.email=test@example.org",
        ])
        .args(args)
        .current_dir(root)
        .env("JJ_CONFIG", "")
        .output()
        .expect("tests require jj 0.46 on PATH");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn fixture() -> TempDir {
    let temp = tempfile::tempdir().unwrap();
    jj(temp.path(), &["git", "init", "--colocate"]);
    jj(temp.path(), &["describe", "-m", "first revision"]);
    temp
}
fn viewer(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jjui-rs"));
    command.arg("-R").arg(root).env("PATH", "");
    command
}
fn heads(root: &Path) -> Vec<String> {
    let mut names = std::fs::read_dir(root.join(".jj/repo/op_heads/heads"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    names
}
struct Watch {
    child: Child,
    updates: mpsc::Receiver<Value>,
}
impl Watch {
    fn new(root: &Path) -> Self {
        Self::with_options(root, &[])
    }
    fn with_options(root: &Path, options: &[&str]) -> Self {
        let mut child = viewer(root)
            .args(options)
            .arg("--watch-json")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, updates) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                if send.send(serde_json::from_str(&line).unwrap()).is_err() {
                    break;
                }
            }
        });
        Self { child, updates }
    }
    fn until(&self, predicate: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let value = self
                .updates
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("live update timed out");
            assert_ne!(value["type"], "error", "{value}");
            if predicate(&value) {
                return value;
            }
        }
    }
}
impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn reads_without_jj_and_does_not_snapshot_working_copy() {
    let temp = fixture();
    let before = heads(temp.path());
    std::fs::write(temp.path().join("unsnapshotted.txt"), "edit").unwrap();
    let output = viewer(temp.path()).arg("--once").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let current = value["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["working_copy"] == true)
        .unwrap();
    assert_eq!(current["author_email"], "test@example.org");
    chrono::NaiveDateTime::parse_from_str(
        current["timestamp"].as_str().unwrap(),
        "%Y-%m-%d %H:%M:%S",
    )
    .unwrap();
    assert!(current["change_prefix_len"].as_u64().unwrap() > 0);
    assert!(current["commit_prefix_len"].as_u64().unwrap() > 0);
    assert_eq!(current["empty"], true);
    assert_eq!(current["root"], false);
    let limited = viewer(temp.path())
        .args(["--once", "--limit", "1"])
        .output()
        .unwrap();
    assert!(limited.status.success());
    let limited: Value = serde_json::from_slice(&limited.stdout).unwrap();
    assert_eq!(
        limited["revisions"][0]["change_prefix_len"],
        current["change_prefix_len"]
    );
    assert_eq!(
        limited["revisions"][0]["commit_prefix_len"],
        current["commit_prefix_len"]
    );
    assert!(
        value["revisions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["description"] == "first revision\n")
    );
    assert_eq!(heads(temp.path()), before);
}

#[test]
fn streams_external_operations_and_unsnapshotted_file_events() {
    let temp = fixture();
    let watch = Watch::new(temp.path());
    let initial = watch.until(|v| v["type"] == "repository");
    let before = heads(temp.path());
    std::fs::write(temp.path().join("live.txt"), "hello").unwrap();
    watch.until(|v| {
        v["type"] == "files"
            && v["paths"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p == "live.txt")
    });
    assert_eq!(heads(temp.path()), before);
    jj(temp.path(), &["new", "-m", "created while watching"]);
    let next = watch.until(|v| {
        v["type"] == "repository" && v["snapshot"]["operation"] != initial["snapshot"]["operation"]
    });
    assert!(
        next["snapshot"]["revisions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["description"] == "created while watching\n")
    );
    jj(temp.path(), &["describe", "-m", "rewritten while watching"]);
    watch.until(|v| {
        v["type"] == "repository"
            && v["snapshot"]["revisions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["description"] == "rewritten while watching\n")
    });
}

#[test]
fn watches_shared_repository_from_secondary_workspace() {
    let temp = fixture();
    let other = temp.path().join("other");
    jj(temp.path(), &["workspace", "add", other.to_str().unwrap()]);
    let watch = Watch::new(&other);
    watch.until(|v| v["type"] == "repository");
    jj(temp.path(), &["new", "-m", "from primary workspace"]);
    watch.until(|v| {
        v["type"] == "repository"
            && v["snapshot"]["revisions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["description"] == "from primary workspace\n")
    });
}

#[test]
fn divergent_operations_are_reported_without_reconciliation() {
    let temp = fixture();
    let before = heads(temp.path());
    let base = &before[0];
    jj(
        temp.path(),
        &["--at-operation", base, "describe", "-m", "branch one"],
    );
    jj(
        temp.path(),
        &["--at-operation", base, "describe", "-m", "branch two"],
    );
    let divergent = heads(temp.path());
    assert_eq!(divergent.len(), 2);
    let output = viewer(temp.path()).arg("--once").output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("operation heads"));
    assert_eq!(heads(temp.path()), divergent);
}

fn snapshot(root: &Path) -> Value {
    let output = viewer(root).arg("--once").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn diff(root: &Path, id: &str) -> Value {
    let output = viewer(root).args(["--diff", id]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn diffs_read_recorded_trees_without_cli_or_operations() {
    let temp = fixture();
    std::fs::write(temp.path().join("text.txt"), "old\n").unwrap();
    std::fs::write(temp.path().join("deleted.txt"), "delete me\n").unwrap();
    jj(temp.path(), &["new", "-m", "changes"]);
    std::fs::write(temp.path().join("text.txt"), "new\n").unwrap();
    std::fs::remove_file(temp.path().join("deleted.txt")).unwrap();
    std::fs::write(temp.path().join("binary.dat"), [0, 1, 2]).unwrap();
    std::fs::write(temp.path().join("large.txt"), vec![b'x'; 300_000]).unwrap();
    jj(temp.path(), &["describe", "-m", "snapshot changes"]);
    let snapshot = snapshot(temp.path());
    let current = snapshot["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["working_copy"] == true)
        .unwrap();
    let heads_before = heads(temp.path());
    std::fs::write(temp.path().join("text.txt"), "UNSNAPSHOTTED\n").unwrap();
    let preview = diff(temp.path(), current["commit_id"].as_str().unwrap());
    let files = preview["files"].as_array().unwrap();
    let patch = |path| {
        files.iter().find(|f| f["path"] == path).unwrap()["patch"]
            .as_str()
            .unwrap()
    };
    assert!(patch("text.txt").contains("-old\n+new\n"));
    assert!(!patch("text.txt").contains("UNSNAPSHOTTED"));
    assert!(patch("binary.dat").contains("binary file"));
    assert!(patch("large.txt").contains("exceeds 256 KiB"));
    assert!(patch("deleted.txt").contains("-delete me"));
    assert_eq!(heads(temp.path()), heads_before);
}

#[test]
fn merge_diff_uses_merged_parents_not_first_parent() {
    let temp = fixture();
    jj(temp.path(), &["bookmark", "create", "base"]);
    jj(temp.path(), &["new", "base", "-m", "left"]);
    std::fs::write(temp.path().join("left.txt"), "left\n").unwrap();
    jj(temp.path(), &["bookmark", "create", "left"]);
    jj(temp.path(), &["new", "base", "-m", "right"]);
    std::fs::write(temp.path().join("right.txt"), "right\n").unwrap();
    jj(temp.path(), &["bookmark", "create", "right"]);
    jj(temp.path(), &["new", "left", "right", "-m", "merge"]);
    let snapshot = snapshot(temp.path());
    let current = snapshot["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["working_copy"] == true)
        .unwrap();
    assert_eq!(current["parents"].as_array().unwrap().len(), 2);
    let preview = diff(temp.path(), current["commit_id"].as_str().unwrap());
    assert!(preview["files"].as_array().unwrap().is_empty());
}

#[test]
fn configurable_keys_validate_overrides_and_report_conflicts() {
    let temp = fixture();
    let config = temp.path().join("custom.toml");
    std::fs::write(&config, "[revisions]\ndown=['ctrl+j']\nnew=[]\n").unwrap();
    let output = viewer(temp.path())
        .arg("--keybinds")
        .arg(&config)
        .arg("--check-keybinds")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("ctrl+j"));
    std::fs::write(&config, "[revisions]\nup=['j']\n").unwrap();
    let output = viewer(temp.path())
        .arg("--keybinds")
        .arg(&config)
        .arg("--check-keybinds")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("bound to both"));
}

#[test]
fn native_revsets_load_repository_aliases_without_jj() {
    let temp = fixture();
    std::fs::write(
        temp.path().join(".jj/repo/config.toml"),
        "[revset-aliases]\n\"mine()\" = \"@\"\n",
    )
    .unwrap();
    let output = viewer(temp.path())
        .args(["--once", "-r", "mine()"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let snapshot: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(snapshot["revisions"].as_array().unwrap().len(), 1);
    assert_eq!(snapshot["revisions"][0]["working_copy"], true);
    let bad = viewer(temp.path())
        .args(["--once", "-r", "this_function_does_not_exist()"])
        .output()
        .unwrap();
    assert!(!bad.status.success());
}

#[test]
fn opt_in_native_snapshot_records_edits_without_jj() {
    let temp = fixture();
    let before = heads(temp.path());
    std::fs::write(
        temp.path().join("native.txt"),
        "snapshotted without a subprocess\n",
    )
    .unwrap();
    let output = viewer(temp.path())
        .args(["--once", "--auto-snapshot"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_ne!(heads(temp.path()), before);
    let snapshot: Value = serde_json::from_slice(&output.stdout).unwrap();
    let wc = snapshot["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["working_copy"] == true)
        .unwrap();
    let preview = diff(temp.path(), wc["commit_id"].as_str().unwrap());
    assert!(
        preview["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == "native.txt")
    );
    let after = heads(temp.path());
    let output = viewer(temp.path())
        .args(["--once", "--auto-snapshot"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        heads(temp.path()),
        after,
        "idle snapshot must not create operations"
    );
}

#[test]
fn auto_snapshot_refuses_immutable_working_copy_without_creating_operation() {
    let temp = fixture();
    std::fs::write(
        temp.path().join(".jj/repo/config.toml"),
        "[revset-aliases]\n\"immutable_heads()\" = \"@\"\n",
    )
    .unwrap();
    let before = heads(temp.path());
    std::fs::write(
        temp.path().join("edit.txt"),
        "do not rewrite immutable commit",
    )
    .unwrap();
    let output = viewer(temp.path())
        .args(["--once", "--auto-snapshot"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("immutable"));
    assert_eq!(heads(temp.path()), before);
}

#[test]
fn native_auto_snapshot_streams_working_tree_edits_without_jj() {
    let temp = fixture();
    let watch = Watch::with_options(temp.path(), &["--auto-snapshot"]);
    let initial = watch.until(|v| v["type"] == "repository");
    std::fs::write(temp.path().join("auto-live.txt"), "live snapshot\n").unwrap();
    let changed = watch.until(|v| {
        v["type"] == "repository" && v["snapshot"]["operation"] != initial["snapshot"]["operation"]
    });
    let current = changed["snapshot"]["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["working_copy"] == true)
        .unwrap();
    let preview = diff(temp.path(), current["commit_id"].as_str().unwrap());
    assert!(
        preview["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == "auto-live.txt")
    );
}

#[test]
fn secure_repo_and_workspace_config_are_loaded_without_migration() {
    let temp = fixture();
    let config_home = temp.path().join(".jj/test-config");
    for (scope, key, value) in [
        ("--repo", "revset-aliases.\"only_current()\"", "@"),
        ("--workspace", "revset-aliases.\"only_root()\"", "root()"),
    ] {
        let output = Command::new("jj")
            .args(["config", "set", scope, key, value])
            .current_dir(temp.path())
            .env("JJ_CONFIG", "")
            .env("XDG_CONFIG_HOME", &config_home)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let before = heads(temp.path());
    for expression in ["only_current()", "only_root()"] {
        let output = viewer(temp.path())
            .env("XDG_CONFIG_HOME", &config_home)
            .args(["--once", "-r", expression])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let snapshot: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(snapshot["revisions"].as_array().unwrap().len(), 1);
    }
    assert_eq!(heads(temp.path()), before);
}

#[test]
fn immutable_flags_follow_configured_heads_without_running_jj() {
    let temp = fixture();
    jj(temp.path(), &["bookmark", "create", "published"]);
    jj(temp.path(), &["new", "-m", "mutable child"]);
    std::fs::write(
        temp.path().join(".jj/repo/config.toml"),
        "[revset-aliases]\n'immutable_heads()' = 'bookmarks(published)'\n",
    )
    .unwrap();
    let before = heads(temp.path());
    let output = viewer(temp.path()).arg("--once").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let snapshot: Value = serde_json::from_slice(&output.stdout).unwrap();
    let revisions = snapshot["revisions"].as_array().unwrap();
    assert!(revisions.iter().any(|r| {
        r["description"]
            .as_str()
            .unwrap()
            .contains("first revision")
            && r["immutable"] == true
    }));
    assert!(
        revisions
            .iter()
            .any(|r| r["working_copy"] == true && r["immutable"] == false)
    );
    assert_eq!(heads(temp.path()), before);
}

#[test]
fn stale_workspace_stays_browsable_and_external_repair_is_detected() {
    let temp = tempfile::tempdir().unwrap();
    let main = temp.path().join("main");
    let other = temp.path().join("other");
    std::fs::create_dir(&main).unwrap();
    let run = |root: &Path, args: &[&str]| {
        let output = Command::new("jj")
            .args([
                "--config",
                "user.name=Test",
                "--config",
                "user.email=test@example.org",
            ])
            .args(args)
            .current_dir(root)
            .env("JJ_CONFIG", "")
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run(&main, &["git", "init", "--colocate"]);
    std::fs::write(main.join("file"), "before\n").unwrap();
    run(&main, &["describe", "-m", "base"]);
    run(
        &main,
        &["workspace", "add", "-r", "@", other.to_str().unwrap()],
    );
    std::fs::write(main.join("file"), "after\n").unwrap();
    run(&main, &["describe", "-m", "rewritten ancestor"]);
    let before = heads(&main);
    let watch = Watch::new(&other);
    watch.until(|v| v["type"] == "repository" && v["snapshot"]["workspace_stale"] == true);
    let auto = Watch::with_options(&other, &["--auto-snapshot"]);
    auto.until(|v| v["type"] == "repository" && v["snapshot"]["workspace_stale"] == true);
    assert_eq!(
        std::fs::read_to_string(other.join("file")).unwrap(),
        "before\n"
    );
    assert_eq!(
        heads(&main),
        before,
        "browsing a stale workspace must not write operations"
    );
    drop(auto);
    // Recovery may only change checkout metadata, not operation heads. The live
    // reader must reload that metadata rather than caching the stale status.
    run(&other, &["workspace", "update-stale"]);
    watch.until(|v| v["type"] == "repository" && v["snapshot"]["workspace_stale"] == false);
    assert_eq!(
        std::fs::read_to_string(other.join("file")).unwrap(),
        "after\n"
    );
}

#[test]
fn auto_snapshot_config_and_cli_precedence_control_real_writes() {
    let temp = fixture();
    let home = temp.path().join(".jj/app-config");
    std::fs::create_dir_all(home.join("jjui-rs")).unwrap();
    std::fs::write(home.join("jjui-rs/config.toml"), "auto-snapshot = true").unwrap();
    let run = |args: &[&str]| {
        let output = viewer(temp.path())
            .env("XDG_CONFIG_HOME", &home)
            .env("JJ_CONFIG", "")
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    std::fs::write(temp.path().join("pending.txt"), "config snapshot\n").unwrap();
    let before = heads(temp.path());
    let output = run(&["--check-config"]);
    assert!(String::from_utf8_lossy(&output.stdout).contains("auto-snapshot = true"));
    assert_eq!(
        heads(temp.path()),
        before,
        "checking configuration must be read-only"
    );
    run(&["--once", "--no-auto-snapshot"]);
    assert_eq!(heads(temp.path()), before);
    run(&["--once"]);
    assert_ne!(
        heads(temp.path()),
        before,
        "global setting did not enable snapshots"
    );
    let before = heads(temp.path());
    std::fs::write(temp.path().join("config.toml"), "auto-snapshot = false").unwrap();
    std::fs::write(temp.path().join("pending.txt"), "second edit\n").unwrap();
    run(&["--once"]);
    assert_eq!(
        heads(temp.path()),
        before,
        "workspace setting did not disable snapshots"
    );
    run(&["--once", "--auto-snapshot"]);
    assert_ne!(
        heads(temp.path()),
        before,
        "CLI did not override workspace configuration"
    );
    let output = viewer(temp.path())
        .args(["--auto-snapshot", "--no-auto-snapshot", "--once"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "contradictory CLI overrides must be rejected"
    );
}
