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
        let mut child = viewer(root)
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
