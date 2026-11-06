use serde_json::{Value, json};
use std::{
    path::Path,
    process::{Command, Output},
};
fn process(root: &Path, program: &str) -> Command {
    let mut c = Command::new(program);
    c.current_dir(root)
        .env("JJ_CONFIG", "")
        .env("XDG_CONFIG_HOME", root.join(".jj/test-config"))
        .env("JJ_USER", "Test")
        .env("JJ_EMAIL", "test@example.org");
    c
}
fn checked(output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
fn jj(root: &Path, args: &[&str]) {
    checked(process(root, "jj").args(args).output().unwrap());
}
fn viewer(root: &Path) -> Command {
    let mut c = process(root, env!("CARGO_BIN_EXE_jjui-rs"));
    c.arg("-R").arg(root);
    c
}
fn snapshot(root: &Path) -> Value {
    serde_json::from_slice(&checked(viewer(root).arg("--once").output().unwrap())).unwrap()
}
fn session(root: &Path, path: &str) -> Value {
    let snapshot = snapshot(root);
    let id = snapshot["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["working_copy"] == true)
        .unwrap()["commit_id"]
        .as_str()
        .unwrap();
    serde_json::from_slice(&checked(
        viewer(root)
            .args(["--chunks", id, path])
            .env("PATH", "")
            .output()
            .unwrap(),
    ))
    .unwrap()
}
fn apply(root: &Path, session: Value, action: &str) -> Output {
    let plan = root.join(".jj/selected-hunks.json");
    std::fs::write(&plan, serde_json::to_vec(&json!({"session":session,"selected":[0],"action":action,"description":"selected hunk"})).unwrap()).unwrap();
    viewer(root)
        .arg("--apply-hunks")
        .arg(plan)
        .output()
        .unwrap()
}
fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    jj(root, &["git", "init", "--colocate"]);
    std::fs::write(root.join("a | b.txt"), "old\nkeep\nold2\n").unwrap();
    jj(root, &["describe", "-m", "baseline"]);
    jj(root, &["new", "-m", "edits"]);
    std::fs::write(root.join("a | b.txt"), "new\nkeep\nnew2\n").unwrap();
    std::fs::write(root.join("unrelated.txt"), "keep this\n").unwrap();
    jj(root, &["util", "snapshot"]);
    temp
}
#[test]
fn native_hunk_restore_keeps_unselected_changes_and_literal_paths() {
    let temp = fixture();
    let root = temp.path();
    let s = session(root, "a | b.txt");
    checked(apply(root, s, "Restore"));
    assert_eq!(
        std::fs::read_to_string(root.join("a | b.txt")).unwrap(),
        "old\nkeep\nnew2\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("unrelated.txt")).unwrap(),
        "keep this\n"
    );
}
#[test]
fn hunk_split_moves_only_the_selected_change() {
    let temp = fixture();
    let root = temp.path();
    let s = session(root, "a | b.txt");
    checked(apply(root, s, "Split"));
    assert_eq!(
        std::fs::read_to_string(root.join("a | b.txt")).unwrap(),
        "new\nkeep\nnew2\n"
    );
    let snapshot = snapshot(root);
    let wc = snapshot["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["working_copy"] == true)
        .unwrap();
    let parent = wc["parents"][0].as_str().unwrap();
    let patch: Value = serde_json::from_slice(&checked(
        viewer(root)
            .args(["--diff", parent])
            .env("PATH", "")
            .output()
            .unwrap(),
    ))
    .unwrap();
    assert_eq!(patch["files"].as_array().unwrap().len(), 1);
    let text = patch["files"][0]["patch"].as_str().unwrap();
    assert!(text.contains("+new\n"));
    assert!(!text.contains("+new2\n"));
    assert!(root.join("unrelated.txt").exists());
}
#[test]
fn edits_after_selection_are_not_overwritten() {
    let temp = fixture();
    let root = temp.path();
    let s = session(root, "a | b.txt");
    std::fs::write(root.join("a | b.txt"), "fresh unsnapshotted work\n").unwrap();
    let output = apply(root, s, "Restore");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("File changed since hunk selection"));
    assert_eq!(
        std::fs::read_to_string(root.join("a | b.txt")).unwrap(),
        "fresh unsnapshotted work\n"
    );
}
#[test]
fn restoring_added_and_deleted_files_preserves_existence() {
    let temp = fixture();
    let root = temp.path();
    checked(apply(root, session(root, "unrelated.txt"), "Restore"));
    assert!(!root.join("unrelated.txt").exists());
    std::fs::remove_file(root.join("a | b.txt")).unwrap();
    jj(root, &["util", "snapshot"]);
    checked(apply(root, session(root, "a | b.txt"), "Restore"));
    assert_eq!(
        std::fs::read_to_string(root.join("a | b.txt")).unwrap(),
        "old\nkeep\nold2\n"
    );
}
