//! Collapse immutable chains without changing branch topology or inventing edges.
use crate::repository::Revision;
use std::collections::{HashMap, HashSet};

/// Merge selected revisions and their omitted ancestors in child-before-parent order.
pub fn with_ancestors(selected: &[Revision], ancestors: &[Revision]) -> Vec<Revision> {
    let revisions: Vec<_> = selected.iter().chain(ancestors).collect();
    let indices: HashMap<_, _> = revisions
        .iter()
        .enumerate()
        .map(|(i, r)| (r.commit_id.as_str(), i))
        .collect();
    let mut children = vec![0usize; revisions.len()];
    for revision in &revisions {
        for parent in &revision.parents {
            if let Some(&i) = indices.get(parent.as_str()) {
                children[i] += 1;
            }
        }
    }
    let mut ready: std::collections::BTreeSet<_> = children
        .iter()
        .enumerate()
        .filter_map(|(i, count)| (*count == 0).then_some(i))
        .collect();
    let mut result = Vec::with_capacity(revisions.len());
    while let Some(i) = ready.pop_first() {
        result.push(revisions[i].clone());
        for parent in &revisions[i].parents {
            if let Some(&p) = indices.get(parent.as_str()) {
                children[p] -= 1;
                if children[p] == 0 {
                    ready.insert(p);
                }
            }
        }
    }
    result
}

pub fn collapse(
    revisions: &[Revision],
    expanded: &HashSet<String>,
    marked: &HashSet<String>,
) -> (Vec<Revision>, HashMap<String, Vec<String>>) {
    let by_id: HashMap<_, _> = revisions
        .iter()
        .map(|r| (r.commit_id.as_str(), r))
        .collect();
    let mut children: HashMap<&str, usize> = HashMap::new();
    for revision in revisions {
        for parent in &revision.parents {
            *children.entry(parent).or_default() += 1;
        }
    }
    let eligible = |r: &Revision| {
        (r.outside_revset
            || (r.immutable
                && !r.log_context
                && !r.root
                && !r.working_copy
                && r.tags.is_empty()
                && (r.bookmarks.is_empty() || r.elide_bookmarks)))
            && !expanded.contains(&r.commit_id)
            && !marked.contains(&r.commit_id)
    };
    let mut consumed = HashSet::new();
    let mut hidden = HashMap::new();
    let mut visible = Vec::new();
    for revision in revisions {
        if consumed.contains(&revision.commit_id) {
            continue;
        }
        if !eligible(revision) {
            visible.push(revision.clone());
            continue;
        }
        let mut group = vec![revision.commit_id.clone()];
        let mut tail = revision;
        // A merge or a shared parent ends the chain. Keeping these junctions
        // separate makes contraction safe even with custom immutable() aliases.
        while tail.parents.len() == 1 {
            let parent = tail.parents[0].as_str();
            let Some(next) = by_id.get(parent) else { break };
            if children.get(parent) != Some(&1)
                || !eligible(next)
                || next.outside_revset != revision.outside_revset
            {
                break;
            }
            group.push(next.commit_id.clone());
            consumed.insert(next.commit_id.clone());
            tail = next;
        }
        if group.len() == 1 {
            visible.push(revision.clone());
            continue;
        }
        let mut placeholder = revision.clone();
        placeholder.parents = tail.parents.clone();
        hidden.insert(revision.commit_id.clone(), group);
        visible.push(placeholder);
    }
    (visible, hidden)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn revision(id: &str, parents: &[&str], immutable: bool) -> Revision {
        Revision {
            commit_id: id.into(),
            parents: parents.iter().map(|p| (*p).into()).collect(),
            immutable,
            ..Default::default()
        }
    }
    #[test]
    fn omitted_merge_ancestors_preserve_topology_and_selected_context() {
        let mut selected = revision("context", &["root"], true);
        selected.log_context = true;
        let mut merge = revision("merge", &["context", "side"], false);
        merge.outside_revset = true;
        let mut side = revision("side", &["root"], false);
        side.outside_revset = true;
        let mut root = revision("root", &[], true);
        root.outside_revset = true;
        root.root = true;
        let history = with_ancestors(
            &[revision("work", &["merge"], false), selected],
            &[merge, side, root],
        );
        assert_eq!(
            history
                .iter()
                .map(|r| r.commit_id.as_str())
                .collect::<Vec<_>>(),
            ["work", "merge", "context", "side", "root"]
        );
        let (visible, hidden) = collapse(&history, &HashSet::new(), &HashSet::new());
        assert!(!hidden.contains_key("context"));
        assert!(hidden.is_empty());
        assert_eq!(visible[1].parents, ["context", "side"]);
    }

    #[test]
    fn default_log_context_stays_visible() {
        let mut context = revision("base", &["older"], true);
        context.log_context = true;
        let revisions = vec![context, revision("older", &[], true)];
        let (_, hidden) = collapse(&revisions, &HashSet::new(), &HashSet::new());
        assert!(!hidden.contains_key("base"));
        assert!(!hidden.contains_key("older"));
    }

    #[test]
    fn contracts_chains_and_expands_without_losing_edges() {
        let revisions = vec![
            revision("work", &["a"], false),
            revision("a", &["b"], true),
            revision("b", &["root"], true),
            Revision {
                root: true,
                ..revision("root", &[], true)
            },
        ];
        let (visible, hidden) = collapse(&revisions, &HashSet::new(), &HashSet::new());
        assert_eq!(
            visible
                .iter()
                .map(|r| r.commit_id.as_str())
                .collect::<Vec<_>>(),
            ["work", "a", "root"]
        );
        assert_eq!(hidden["a"], ["a", "b"]);
        assert_eq!(visible[1].parents, ["root"]);
        let expanded = hidden["a"].iter().cloned().collect();
        let (visible, hidden) = collapse(&revisions, &expanded, &HashSet::new());
        assert_eq!(visible.len(), 4);
        assert!(hidden.is_empty());
        assert_eq!(visible[1].parents, ["b"]);
    }
    #[test]
    fn bookmark_targets_split_immutable_chains_without_hiding_the_targets() {
        let mut revisions = vec![
            revision("newer", &["local"], true),
            revision("local", &["middle"], true),
            revision("middle", &["remote"], true),
            revision("remote", &["older"], true),
            revision("older", &["oldest"], true),
            revision("oldest", &[], true),
        ];
        revisions[1].bookmarks = vec!["main*".into()];
        revisions[3].bookmarks = vec!["main@origin".into()];
        let (visible, hidden) = collapse(&revisions, &HashSet::new(), &HashSet::new());
        assert!(!hidden.contains_key("newer"));
        assert!(!hidden.contains_key("middle"));
        assert_eq!(hidden["older"], ["older", "oldest"]);
        for id in ["local", "remote"] {
            assert!(!hidden.values().flatten().any(|hidden_id| hidden_id == id));
            let target = visible
                .iter()
                .find(|revision| revision.commit_id == id)
                .unwrap();
            let original = revisions
                .iter()
                .find(|revision| revision.commit_id == id)
                .unwrap();
            assert_eq!(target.bookmarks, original.bookmarks);
            assert_eq!(target.parents, original.parents);
        }
        assert_eq!(visible[0].parents, ["local"]);
        assert_eq!(visible[2].parents, ["remote"]);
    }

    #[test]
    fn old_untracked_bookmarks_fold_and_reappear_on_expansion() {
        let mut revisions = vec![
            revision("base", &["old"], true),
            revision("old", &["older"], true),
            revision("older", &[], true),
        ];
        revisions[0].bookmarks = vec!["main@origin".into()];
        revisions[1].bookmarks = vec!["old-topic@origin".into()];
        revisions[1].elide_bookmarks = true;
        let (visible, hidden) = collapse(&revisions, &HashSet::new(), &HashSet::new());
        assert_eq!(visible.len(), 2);
        assert!(!hidden.contains_key("base"));
        assert_eq!(hidden["old"], ["old", "older"]);
        let expanded = hidden["old"].iter().cloned().collect();
        let (visible, hidden) = collapse(&revisions, &expanded, &HashSet::new());
        assert!(hidden.is_empty());
        assert_eq!(visible[1].bookmarks, ["old-topic@origin"]);
        let marked = HashSet::from(["old".to_owned()]);
        let (_, hidden) = collapse(&revisions, &HashSet::new(), &marked);
        assert!(!hidden.contains_key("old"));
    }

    #[test]
    fn shared_ancestors_and_mutable_branches_keep_their_junctions() {
        let revisions = vec![
            revision("merge", &["left", "right"], true),
            revision("left", &["base"], true),
            revision("right", &["base"], false),
            revision("base", &["older"], true),
            revision("older", &[], true),
        ];
        let (visible, hidden) = collapse(&revisions, &HashSet::new(), &HashSet::new());
        assert_eq!(hidden["base"], ["base", "older"]);
        assert!(!hidden.contains_key("merge"));
        assert!(!hidden.contains_key("left"));
        assert_eq!(visible[0].parents, ["left", "right"]);
        assert_eq!(visible[1].parents, ["base"]);
        assert_eq!(visible[2].parents, ["base"]);
        assert!(
            crate::graph::render(&visible)
                .last()
                .unwrap()
                .boundary
                .is_none()
        );
    }
}
