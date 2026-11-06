//! Collapse immutable chains without changing branch topology or inventing edges.
use crate::repository::Revision;
use std::collections::{HashMap, HashSet};

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
        r.immutable
            && !r.root
            && !r.working_copy
            && r.tags.is_empty()
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
            if children.get(parent) != Some(&1) || !eligible(next) {
                break;
            }
            group.push(next.commit_id.clone());
            consumed.insert(next.commit_id.clone());
            tail = next;
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
