//! Stable lanes: each active lane names a pending ancestor. Interior lanes are
//! retained until reused, so unrelated edges never shift under a revision.
use crate::repository::Revision;

#[derive(Debug, Default)]
pub struct GraphRow {
    pub node: String,
    pub edge: String,
    /// Active ancestor lanes below the edge transition.
    pub continuation: String,
    pub boundary: Option<String>,
}

pub fn render(revisions: &[Revision]) -> Vec<GraphRow> {
    let mut lanes: Vec<Option<&str>> = Vec::new();
    let mut result = Vec::new();
    for revision in revisions {
        let lane = lanes
            .iter()
            .position(|id| *id == Some(&revision.commit_id))
            .unwrap_or_else(|| allocate(&mut lanes, &revision.commit_id));
        let before = lanes.clone();
        lanes[lane] = None;
        let mut targets = Vec::new();
        for parent in &revision.parents {
            let target = lanes
                .iter()
                .position(|id| *id == Some(parent.as_str()))
                .unwrap_or_else(|| allocate(&mut lanes, parent));
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
        let width = before.len().max(lanes.len());
        let mut node = String::new();
        let mut edge = String::new();
        let left = targets.iter().copied().chain([lane]).min().unwrap();
        let right = targets.iter().copied().chain([lane]).max().unwrap();
        for col in 0..width {
            node.push(if col == lane {
                if revision.working_copy { '@' } else { '○' }
            } else if before.get(col).is_some_and(Option::is_some) {
                '│'
            } else {
                ' '
            });
            node.push(' ');
            let up = before.get(col).is_some_and(Option::is_some)
                && (col != lane || !targets.is_empty());
            let down = lanes.get(col).is_some_and(Option::is_some);
            let west = col > left && col <= right;
            let east = col >= left && col < right;
            let crossing = up && down && west && east && col != lane && !targets.contains(&col);
            edge.push(if crossing {
                '╪'
            } else {
                glyph(up, down, west, east)
            });
            edge.push(if east { '─' } else { ' ' });
        }
        result.push(GraphRow {
            node,
            edge,
            continuation: (0..width)
                .map(|col| {
                    if lanes.get(col).is_some_and(Option::is_some) {
                        "│ "
                    } else {
                        "  "
                    }
                })
                .collect(),
            boundary: None,
        });
        while lanes.last() == Some(&None) {
            lanes.pop();
        }
    }
    if let Some(last) = result.last_mut()
        && lanes.iter().any(Option::is_some)
    {
        last.boundary = Some(
            lanes
                .iter()
                .map(|id| if id.is_some() { "~ " } else { "  " })
                .collect(),
        );
    }
    result
}

fn allocate<'a>(lanes: &mut Vec<Option<&'a str>>, id: &'a str) -> usize {
    if let Some(index) = lanes.iter().position(Option::is_none) {
        lanes[index] = Some(id);
        index
    } else {
        lanes.push(Some(id));
        lanes.len() - 1
    }
}

fn glyph(up: bool, down: bool, left: bool, right: bool) -> char {
    match (up, down, left, right) {
        (false, false, false, false) => ' ',
        (true, true, false, false) | (true, false, false, false) | (false, true, false, false) => {
            '│'
        }
        (false, false, _, _) => '─',
        (true, false, false, true) => '└',
        (true, false, true, false) => '┘',
        (false, true, false, true) => '┌',
        (false, true, true, false) => '┐',
        (true, true, false, true) => '├',
        (true, true, true, false) => '┤',
        (true, false, true, true) => '┴',
        (false, true, true, true) => '┬',
        (true, true, true, true) => '┼',
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rev(id: &str, parents: &[&str]) -> Revision {
        Revision {
            commit_id: id.into(),
            change_id: id.into(),
            parents: parents.iter().map(|s| (*s).into()).collect(),
            description: String::new(),
            author: String::new(),
            bookmarks: vec![],
            working_copy: false,
        }
    }
    #[test]
    fn linear_history_and_truncation() {
        let rows = render(&[rev("a", &["b"]), rev("b", &["root"])]);
        assert_eq!(rows[0].node, "○ ");
        assert_eq!(rows[0].edge, "│ ");
        assert_eq!(rows[1].boundary.as_deref(), Some("~ "));
    }
    #[test]
    fn diamond_splits_and_rejoins() {
        let rows = render(&[
            rev("merge", &["a", "b"]),
            rev("a", &["root"]),
            rev("b", &["root"]),
            rev("root", &[]),
        ]);
        assert_eq!(rows[0].edge, "├─┐ ");
        assert_eq!(rows[0].continuation, "│ │ ");
        assert_eq!(rows[1].node, "○ │ ");
        assert_eq!(rows[2].node, "│ ○ ");
        assert_eq!(rows[2].edge, "├─┘ ");
        assert_eq!(rows[3].node, "○ ");
        assert!(rows[3].boundary.is_none());
    }
    #[test]
    fn separate_heads_join_shared_ancestor() {
        let rows = render(&[rev("a", &["root"]), rev("b", &["root"]), rev("root", &[])]);
        assert_eq!(rows[1].node, "│ ○ ");
        assert_eq!(rows[1].edge, "├─┘ ");
        assert!(render(&[]).is_empty());
    }
}
