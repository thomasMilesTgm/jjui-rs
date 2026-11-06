use crate::{
    live::{Live, Update},
    repository::Snapshot,
};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, List, ListItem, ListState, Paragraph},
};
use std::{
    collections::{HashSet, VecDeque},
    time::Duration,
};

#[derive(Default)]
struct App {
    keys: crate::keybinds::Keybinds,
    input: Option<crate::input::Input>,
    target: Option<(String, Vec<String>, Vec<String>)>,
    busy: bool,
    pending_command: bool,
    follow_working_copy: bool,
    output: String,
    listing: Vec<crate::browser::Row>,
    operation_graph: Vec<crate::graph::GraphRow>,
    operation_diff: Option<OperationDiff>,
    operation_selection: ListState,
    operation_hit_rows: Vec<(ratatui::layout::Rect, usize)>,
    browse_index: usize,
    file_filter: String,
    marked_files: HashSet<String>,
    revset: String,
    history: Vec<String>,
    hit_rows: Vec<(ratatui::layout::Rect, usize, Option<usize>)>,
    snapshot: Option<Snapshot>,
    selection: ListState,
    graph_selection: ListState,
    files: VecDeque<String>,
    error: Option<String>,
    filter: String,
    searching: bool,
    notice: Option<String>,
    graph: Vec<crate::graph::GraphRow>,
    preview: Option<crate::preview::Preview>,
    requested: Option<String>,
    preview_error: Option<String>,
    preview_visible: bool,
    preview_bottom: bool,
    preview_focus: bool,
    view: View,
    file_index: usize,
    diff_file: bool,
    scroll: u16,
    marked: HashSet<String>,
}

#[derive(Clone, Copy, Default, Debug, PartialEq)]
enum View {
    #[default]
    Revisions,
    Files,
    Diff,
    Bookmarks,
    Help,
    Operations,
    InlineOperations,
    OperationDiff,
    Git,
    Text,
    Finder,
}

struct OperationDiff {
    id: String,
    origin: View,
    result: Option<Result<String, String>>,
}

#[derive(Debug, PartialEq)]
enum Effect {
    None,
    Quit,
    Refresh,
    Request(crate::live::Request),
    External(crate::commands::CommandSpec),
    Shell(String),
}

impl App {
    fn selected_id(&self) -> Option<&str> {
        self.snapshot
            .as_ref()?
            .revisions
            .get(self.selection.selected()?)
            .map(|r| r.commit_id.as_str())
    }

    fn preview_text(&self) -> String {
        if let Some(error) = &self.preview_error {
            return format!("Cannot load diff: {error}");
        }
        let file = (self.view == View::Files || self.diff_file && self.view == View::Diff)
            .then_some(self.file_index);
        let patch = self
            .preview
            .as_ref()
            .map(|p| p.patch(file))
            .unwrap_or_else(|| "Loading diff…".into());
        if file.is_none()
            && let Some(r) = self
                .snapshot
                .as_ref()
                .and_then(|s| self.selection.selected().and_then(|i| s.revisions.get(i)))
        {
            format!(
                "Change {}\nCommit {}\nAuthor {}\nParents {}\n\n{}\n\n{patch}",
                r.change_id,
                r.commit_id,
                r.author,
                r.parents
                    .iter()
                    .map(|id| short(id))
                    .collect::<Vec<_>>()
                    .join(", "),
                r.description
            )
        } else {
            patch
        }
    }

    fn visible(&self) -> Vec<usize> {
        (0..self.snapshot.as_ref().map_or(0, |s| s.revisions.len())).collect()
    }

    fn matches(&self, index: usize) -> bool {
        if self.filter.is_empty() {
            return false;
        }
        let r = &self.snapshot.as_ref().unwrap().revisions[index];
        format!(
            "{} {} {} {}",
            r.change_id,
            r.commit_id,
            r.description,
            r.bookmarks.join(" ")
        )
        .to_lowercase()
        .contains(&self.filter.to_lowercase())
    }

    fn search_match(&mut self, backwards: bool, include_current: bool) {
        let len = self.visible().len();
        if len == 0 || self.filter.is_empty() {
            return;
        }
        let current = self.selection.selected().unwrap_or(0);
        let start = usize::from(!include_current);
        for offset in start..len + start {
            let index = if backwards {
                (current + len - offset % len) % len
            } else {
                (current + offset) % len
            };
            if self.matches(index) {
                self.selection.select(Some(index));
                return;
            }
        }
    }

    fn sources(&self) -> Vec<String> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        if !self.marked.is_empty() {
            return snapshot
                .revisions
                .iter()
                .filter(|r| self.marked.contains(&r.commit_id))
                .map(|r| r.change_id.clone())
                .collect();
        }
        self.selection
            .selected()
            .and_then(|i| snapshot.revisions.get(i))
            .map(|r| vec![r.change_id.clone()])
            .unwrap_or_default()
    }
    fn action_sources(&self, action: &str) -> Vec<String> {
        if matches!(
            action,
            "new" | "rebase" | "squash" | "abandon" | "duplicate" | "push"
        ) {
            return self.sources();
        }
        self.snapshot
            .as_ref()
            .and_then(|s| self.selection.selected().and_then(|i| s.revisions.get(i)))
            .map(|r| vec![r.change_id.clone()])
            .unwrap_or_default()
    }
    fn selected_files(&self) -> Vec<String> {
        if self.view != View::Files {
            return Vec::new();
        }
        if !self.marked_files.is_empty() {
            return self.marked_files.iter().cloned().collect();
        }
        self.preview
            .as_ref()
            .and_then(|p| p.files.get(self.file_index))
            .map(|f| vec![f.path.clone()])
            .unwrap_or_default()
    }
    fn start_input(&mut self, action: &str, label: &str, value: String, confirm: bool) {
        let mut input =
            crate::input::Input::new(action, label, value, self.sources(), self.selected_files());
        input.confirm = confirm;
        self.input = Some(input);
        self.error = None;
        self.notice = None;
    }
    fn command_effect(&mut self, spec: crate::commands::CommandSpec) -> Effect {
        self.history.push(spec.display());
        if self.history.len() > 100 {
            self.history.remove(0);
        }
        self.target = None;
        if spec.interactive {
            Effect::External(spec)
        } else {
            self.busy = true;
            self.pending_command = true;
            Effect::Request(crate::live::Request::Command(spec))
        }
    }
    fn handle_key(&mut self, key: KeyEvent, page_size: usize) -> Effect {
        if key.kind == KeyEventKind::Release {
            return Effect::None;
        }
        if self.input.is_some() {
            let action = self.keys.resolve("input", key);
            if action.as_deref() == Some("cancel") {
                self.input = None;
                return Effect::None;
            }
            if matches!(action.as_deref(), Some("submit" | "accept")) {
                let input = self.input.take().unwrap();
                match input.action.as_str() {
                    "revset" => {
                        self.busy = true;
                        return Effect::Request(crate::live::Request::Revset(input.text));
                    }
                    "search" => {
                        self.filter = input.text;
                        self.search_match(false, true);
                        return Effect::None;
                    }
                    "file_filter" => {
                        self.file_filter = input.text;
                        self.browse_index = 0;
                        return Effect::None;
                    }
                    "shell" => return Effect::Shell(input.text),
                    _ => match crate::commands::action(
                        &input.action,
                        &input.sources,
                        &input.text,
                        &input.files,
                    ) {
                        Ok(spec) => return self.command_effect(spec),
                        Err(error) => {
                            self.error = Some(format!("{error:#}"));
                            self.input = Some(input);
                            return Effect::None;
                        }
                    },
                }
            }
            let input = self.input.as_mut().unwrap();
            if !input.confirm {
                match action.as_deref() {
                    Some(
                        "backspace" | "delete" | "cursor_left" | "cursor_right" | "input_home"
                        | "input_end" | "newline",
                    ) => input.edit(action.as_deref().unwrap()),
                    _ => {
                        if let KeyCode::Char(c) = key.code
                            && !key.modifiers.intersects(
                                KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                            )
                        {
                            input.insert(c);
                        }
                    }
                }
                if input.action == "search" {
                    self.filter = input.text.clone();
                    self.search_match(false, true);
                } else if input.action == "file_filter" {
                    self.file_filter = input.text.clone();
                    self.browse_index = 0;
                }
            }
            return Effect::None;
        }
        if self.target.is_some() && self.keys.resolve("input", key).as_deref() == Some("submit") {
            let (action, sources, files) = self.target.clone().unwrap();
            if let Some(destination) = self.sources().first() {
                // Use the cursor, not the marked source set, as the destination.
                let destination = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| self.selection.selected().and_then(|i| s.revisions.get(i)))
                    .map(|r| r.change_id.as_str())
                    .unwrap_or(destination);
                match crate::commands::action(&action, &sources, destination, &files) {
                    Ok(spec) => return self.command_effect(spec),
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            return Effect::None;
        }
        let scope = match self.view {
            View::Revisions => "revisions",
            View::Files => "files",
            View::Bookmarks => "bookmarks",
            View::Operations | View::InlineOperations => "operations",
            View::Git => "git",
            View::Finder => "finder",
            _ => "view",
        };
        let Some(action) = self.keys.resolve(scope, key) else {
            return Effect::None;
        };
        if action == "diff" && matches!(self.view, View::Operations | View::InlineOperations) {
            let Some(row) = self.listing.get(self.browse_index) else {
                return Effect::None;
            };
            if !row.operation.as_ref().is_some_and(|op| op.snapshot) {
                self.notice = Some("Diff is available for snapshot working-copy operations".into());
                return Effect::None;
            }
            let id = row.id.clone();
            self.operation_diff = Some(OperationDiff {
                id: id.clone(),
                origin: self.view,
                result: None,
            });
            self.view = View::OperationDiff;
            self.scroll = 0;
            self.notice = None;
            self.error = None;
            self.busy = true;
            return Effect::Request(crate::live::Request::OperationDiff(id));
        }
        if action == "cancel" && self.view == View::OperationDiff {
            self.view = self
                .operation_diff
                .take()
                .map_or(View::Revisions, |diff| diff.origin);
            self.scroll = 0;
            let query = if self.view == View::Operations {
                Some(crate::browser::Query::Operations)
            } else {
                self.selected_id()
                    .map(|id| crate::browser::Query::CommitOperations(id.into()))
            };
            if let Some(query) = query {
                self.busy = true;
                return Effect::Request(crate::live::Request::Listing(query));
            }
            return Effect::None;
        }
        if self.pending_command
            && matches!(
                action.as_str(),
                "new"
                    | "describe"
                    | "describe_editor"
                    | "edit"
                    | "rebase"
                    | "squash"
                    | "abandon"
                    | "absorb"
                    | "duplicate"
                    | "split"
                    | "diff_edit"
                    | "commit"
                    | "restore"
                    | "set_bookmark"
                    | "undo"
                    | "redo"
                    | "snapshot"
                    | "fetch"
                    | "push"
                    | "remote_add"
                    | "remote_remove"
                    | "operation_restore"
                    | "bookmark_delete"
                    | "bookmark_track"
                    | "bookmark_untrack"
                    | "command"
                    | "shell"
            )
        {
            self.notice = Some("An action is still running".into());
            return Effect::None;
        }
        if action == "help" {
            self.view = View::Help;
            self.scroll = 0;
            return Effect::None;
        }
        if action == "preview" {
            self.preview_visible = !self.preview_visible;
            self.preview_focus = false;
            return Effect::None;
        }
        if action == "preview_position" {
            self.preview_visible = true;
            self.preview_bottom = !self.preview_bottom;
            return Effect::None;
        }
        if action == "focus" && self.preview_visible {
            self.preview_focus = !self.preview_focus;
            return Effect::None;
        }
        if self.preview_focus
            && self.preview_visible
            && matches!(action.as_str(), "up" | "down" | "page_up" | "page_down")
        {
            let amount = if action.starts_with("page_") {
                page_size as u16
            } else {
                1
            };
            self.scroll = if action.ends_with("down") {
                self.scroll.saturating_add(amount)
            } else {
                self.scroll.saturating_sub(amount)
            };
            return Effect::None;
        }
        if self.view == View::Files
            && self.file_indices().is_empty()
            && self.marked_files.is_empty()
            && matches!(
                action.as_str(),
                "restore" | "split" | "squash" | "absorb" | "diff" | "select"
            )
        {
            self.notice = Some("No matching files selected".into());
            return Effect::None;
        }
        if self.view == View::Files
            && matches!(
                action.as_str(),
                "down" | "up" | "page_down" | "page_up" | "first" | "last"
            )
        {
            let indices = self.file_indices();
            if !indices.is_empty() {
                let current = indices
                    .iter()
                    .position(|i| *i == self.file_index)
                    .unwrap_or(0);
                let next = match action.as_str() {
                    "down" => (current + 1).min(indices.len() - 1),
                    "up" => current.saturating_sub(1),
                    "page_down" => (current + page_size).min(indices.len() - 1),
                    "page_up" => current.saturating_sub(page_size),
                    "last" => indices.len() - 1,
                    _ => 0,
                };
                self.file_index = indices[next];
                self.scroll = 0;
            }
            return Effect::None;
        }
        if matches!(
            self.view,
            View::Operations | View::InlineOperations | View::Bookmarks | View::Finder
        ) {
            let count = self.listing_rows().len();
            match action.as_str() {
                "down" => {
                    self.browse_index = (self.browse_index + 1).min(count.saturating_sub(1));
                    return Effect::None;
                }
                "up" => {
                    self.browse_index = self.browse_index.saturating_sub(1);
                    return Effect::None;
                }
                "page_down" => {
                    self.browse_index =
                        (self.browse_index + page_size).min(count.saturating_sub(1));
                    return Effect::None;
                }
                "page_up" => {
                    self.browse_index = self.browse_index.saturating_sub(page_size);
                    return Effect::None;
                }
                "first" => {
                    self.browse_index = 0;
                    return Effect::None;
                }
                "last" => {
                    self.browse_index = count.saturating_sub(1);
                    return Effect::None;
                }
                _ => {}
            }
        }
        match action.as_str() {
            "search" => {
                if self.view == View::Files || self.view == View::Finder {
                    self.start_input(
                        "file_filter",
                        "Filter files",
                        self.file_filter.clone(),
                        false,
                    );
                } else {
                    self.start_input("search", "Quick search", self.filter.clone(), false);
                }
                return Effect::None;
            }
            "revset" => {
                self.start_input("revset", "Revision expression", self.revset.clone(), false);
                return Effect::None;
            }
            "describe" => {
                let text = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| self.selection.selected().and_then(|i| s.revisions.get(i)))
                    .map(|r| r.description.clone())
                    .unwrap_or_default();
                self.start_input("describe", "Description", text, false);
                return Effect::None;
            }
            "commit" => {
                self.start_input(
                    "commit",
                    "Commit working copy: description",
                    String::new(),
                    false,
                );
                return Effect::None;
            }
            "set_bookmark" => {
                self.start_input(
                    &action,
                    "Set bookmark at selected revision",
                    String::new(),
                    false,
                );
                return Effect::None;
            }
            "rebase" | "squash" => {
                self.target = Some((action.clone(), self.sources(), self.selected_files()));
                self.view = View::Revisions;
                self.notice = Some(format!(
                    "{action}: choose destination, {} apply, {} cancel",
                    self.keys.label("input", "submit"),
                    self.keys.label("global", "cancel")
                ));
                return Effect::None;
            }
            "abandon" | "restore" | "undo" | "redo" => {
                self.start_input(
                    &action,
                    &format!("Confirm {action} for {} revision(s)", self.sources().len()),
                    String::new(),
                    true,
                );
                return Effect::None;
            }
            "new" | "edit" | "describe_editor" | "diff_edit" | "duplicate" | "absorb"
            | "snapshot" => {
                match crate::commands::action(
                    &action,
                    &self.action_sources(&action),
                    "",
                    &self.selected_files(),
                ) {
                    Ok(spec) => return self.command_effect(spec),
                    Err(e) => self.error = Some(e.to_string()),
                }
                return Effect::None;
            }
            "split" => {
                if self.selected_files().is_empty() {
                    match crate::commands::action("split", &self.sources(), "", &[]) {
                        Ok(spec) => return self.command_effect(spec),
                        Err(e) => self.error = Some(e.to_string()),
                    }
                } else {
                    self.start_input("split", "Description for split files", String::new(), false);
                }
                return Effect::None;
            }
            "operations" | "full_operations" => {
                if action == "operations" && self.view == View::InlineOperations {
                    self.view = View::Revisions;
                    return Effect::None;
                }
                let query = if action == "full_operations" {
                    self.view = View::Operations;
                    crate::browser::Query::Operations
                } else if let Some(id) = self.selected_id().map(str::to_owned) {
                    self.view = View::InlineOperations;
                    crate::browser::Query::CommitOperations(id)
                } else {
                    return Effect::None;
                };
                self.operation_graph.clear();
                self.operation_selection = ListState::default();
                self.listing.clear();
                self.browse_index = 0;
                self.busy = true;
                return Effect::Request(crate::live::Request::Listing(query));
            }
            "bookmarks" => {
                self.view = View::Bookmarks;
                self.listing.clear();
                self.browse_index = 0;
                self.busy = true;
                return Effect::Request(crate::live::Request::Listing(
                    crate::browser::Query::Bookmarks,
                ));
            }
            "file_search" => {
                if let Some(id) = self.selected_id().map(str::to_owned) {
                    self.view = View::Finder;
                    self.listing.clear();
                    self.file_filter.clear();
                    self.browse_index = 0;
                    self.busy = true;
                    return Effect::Request(crate::live::Request::Listing(
                        crate::browser::Query::Files(id),
                    ));
                }
                return Effect::None;
            }
            "evolution" => {
                self.view = View::Text;
                match crate::commands::action(&action, &self.sources(), "", &[]) {
                    Ok(spec) => return self.command_effect(spec),
                    Err(e) => self.error = Some(e.to_string()),
                }
                return Effect::None;
            }
            "git" => {
                self.view = View::Git;
                return self.command_effect(crate::commands::CommandSpec::new([
                    "git",
                    "remote",
                    "list",
                    "--ignore-working-copy",
                ]));
            }
            "fetch" | "push" => {
                self.start_input(
                    &action,
                    if action == "push" {
                        "Push to remote (publishes bookmarks)"
                    } else {
                        "Fetch remote"
                    },
                    "origin".into(),
                    false,
                );
                return Effect::None;
            }
            "remote_add" => {
                self.start_input(&action, "Remote name and URL", String::new(), false);
                return Effect::None;
            }
            "remote_remove" => {
                self.start_input(&action, "Remove remote: name", String::new(), false);
                return Effect::None;
            }
            "operation_restore" | "bookmark_delete" | "bookmark_track" | "bookmark_untrack" => {
                if let Some(row) = self.listing_rows().get(self.browse_index) {
                    let value = row.id.clone();
                    let label = if action == "operation_restore" {
                        format!("Restore entire repository to operation {value}")
                    } else {
                        format!("Confirm {action}: {value}")
                    };
                    self.start_input(&action, &label, value, true);
                }
                return Effect::None;
            }
            "command" => {
                self.start_input("command", "jj command (no jj log)", String::new(), false);
                return Effect::None;
            }
            "shell" => {
                self.start_input("shell", "Shell command", String::new(), false);
                return Effect::None;
            }
            "history" => {
                self.output = self.history.join("\n");
                self.view = View::Text;
                self.scroll = 0;
                return Effect::None;
            }
            "select" if self.view == View::Files => {
                if let Some(path) = self
                    .preview
                    .as_ref()
                    .and_then(|p| p.files.get(self.file_index))
                    .map(|f| f.path.clone())
                    && !self.marked_files.remove(&path)
                {
                    self.marked_files.insert(path);
                }
                return Effect::None;
            }
            "cancel" => {
                self.target = None;
                self.marked_files.clear();
                self.file_filter.clear();
            }
            _ => {}
        }
        if self.view == View::Finder
            && action == "open_file"
            && let Some(row) = self.listing_rows().get(self.browse_index)
        {
            let path = format!("root-file:{}", serde_json::to_string(&row.id).unwrap());
            let source = self.sources().first().cloned().unwrap_or("@".into());
            self.view = View::Text;
            return self.command_effect(crate::commands::CommandSpec::new([
                "file",
                "show",
                "-r",
                &source,
                "--ignore-working-copy",
                "--",
                &path,
            ]));
        }

        let legacy = match action.as_str() {
            "quit" => KeyCode::Char('q'),
            "cancel" => KeyCode::Esc,
            "help" => KeyCode::F(1),
            "down" => KeyCode::Down,
            "up" => KeyCode::Up,
            "page_down" => KeyCode::PageDown,
            "page_up" => KeyCode::PageUp,
            "first" => KeyCode::Home,
            "last" => KeyCode::End,
            "working_copy" => KeyCode::Char('@'),
            "parent" => KeyCode::Char('J'),
            "child" => KeyCode::Char('K'),
            "select" => KeyCode::Char(' '),
            "next_match" => KeyCode::Char('\''),
            "previous_match" => KeyCode::Char('"'),
            "details" => KeyCode::Right,
            "diff" => KeyCode::Char('d'),
            "preview" => KeyCode::Char('p'),
            "preview_position" => KeyCode::Char('P'),
            "focus" => KeyCode::Tab,
            "refresh" | "preview_down" | "preview_up" | "preview_page_down" | "preview_page_up" => {
                let c = match action.as_str() {
                    "refresh" => 'r',
                    "preview_down" => 'n',
                    "preview_up" => 'p',
                    "preview_page_down" => 'd',
                    _ => 'u',
                };
                return self.handle_navigation(
                    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL),
                    page_size,
                );
            }
            _ => return Effect::None,
        };
        self.handle_navigation(KeyEvent::new(legacy, KeyModifiers::NONE), page_size)
    }
    fn file_indices(&self) -> Vec<usize> {
        self.preview
            .as_ref()
            .map(|p| {
                p.files
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| fuzzy_match(&f.path, &self.file_filter))
                    .map(|(i, _)| i)
                    .collect()
            })
            .unwrap_or_default()
    }
    fn mouse(&mut self, mouse: event::MouseEvent) {
        if self.input.is_some() {
            return;
        }
        match mouse.kind {
            event::MouseEventKind::Down(event::MouseButton::Left) => {
                if let Some((_, index)) = self
                    .operation_hit_rows
                    .iter()
                    .find(|(rect, _)| rect.contains((mouse.column, mouse.row).into()))
                {
                    self.browse_index = *index;
                    return;
                }
                if let Some((_, revision, file)) = self
                    .hit_rows
                    .iter()
                    .find(|(rect, _, _)| rect.contains((mouse.column, mouse.row).into()))
                    .copied()
                {
                    self.selection.select(Some(revision));
                    if let Some(file) = file {
                        self.view = View::Files;
                        self.file_index = file;
                    } else {
                        self.view = View::Revisions;
                        if mouse.modifiers.contains(KeyModifiers::CONTROL)
                            && let Some(id) = self.selected_id().map(str::to_owned)
                            && !self.marked.remove(&id)
                        {
                            self.marked.insert(id);
                        }
                    }
                }
            }
            event::MouseEventKind::ScrollDown | event::MouseEventKind::ScrollUp => {
                let down = mouse.kind == event::MouseEventKind::ScrollDown;
                if matches!(self.view, View::Operations | View::InlineOperations) {
                    self.browse_index = if down {
                        (self.browse_index + 1).min(self.listing.len().saturating_sub(1))
                    } else {
                        self.browse_index.saturating_sub(1)
                    };
                } else if self.view == View::Files {
                    let indices = self.file_indices();
                    if !indices.is_empty() {
                        let i = indices
                            .iter()
                            .position(|i| *i == self.file_index)
                            .unwrap_or(0);
                        self.file_index = indices[if down {
                            (i + 1).min(indices.len() - 1)
                        } else {
                            i.saturating_sub(1)
                        }];
                    }
                } else {
                    self.handle_navigation(
                        KeyEvent::new(
                            if down { KeyCode::Down } else { KeyCode::Up },
                            KeyModifiers::NONE,
                        ),
                        1,
                    );
                }
            }
            _ => {}
        }
    }
    fn apply_operation_diff(&mut self, id: &str, result: Result<String, String>) {
        if self.view == View::OperationDiff
            && let Some(diff) = &mut self.operation_diff
            && diff.id == id
        {
            diff.result = Some(result);
        }
    }
    fn accepts_listing(&self, query: &crate::browser::Query) -> bool {
        match query {
            crate::browser::Query::Bookmarks => self.view == View::Bookmarks,
            crate::browser::Query::Operations => self.view == View::Operations,
            crate::browser::Query::CommitOperations(id) => {
                self.view == View::InlineOperations && self.selected_id() == Some(id)
            }
            crate::browser::Query::Files(id) => {
                self.view == View::Finder && self.selected_id() == Some(id)
            }
        }
    }
    fn listing_rows(&self) -> Vec<&crate::browser::Row> {
        self.listing
            .iter()
            .filter(|row| self.view != View::Finder || fuzzy_match(&row.label, &self.file_filter))
            .collect()
    }
    fn handle_navigation(&mut self, key: KeyEvent, page_size: usize) -> Effect {
        if key.kind == KeyEventKind::Release {
            return Effect::None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('c') => Effect::Quit,
                KeyCode::Char('r') if !self.searching => {
                    self.requested = None;
                    Effect::Refresh
                }
                KeyCode::Char('n' | 'd') if !self.searching && self.preview_visible => {
                    self.scroll = self
                        .scroll
                        .saturating_add(if key.code == KeyCode::Char('d') {
                            page_size as u16
                        } else {
                            1
                        });
                    Effect::None
                }
                KeyCode::Char('p' | 'u') if !self.searching && self.preview_visible => {
                    self.scroll = self
                        .scroll
                        .saturating_sub(if key.code == KeyCode::Char('u') {
                            page_size as u16
                        } else {
                            1
                        });
                    Effect::None
                }
                _ => Effect::None,
            };
        }
        if key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            return Effect::None;
        }
        if self.searching {
            match key.code {
                KeyCode::Esc => {
                    self.filter.clear();
                    self.searching = false;
                }
                KeyCode::Enter => self.searching = false,
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(c) => self.filter.push(c),
                _ => {}
            }
            self.search_match(false, true);
            return Effect::None;
        }
        let len = self.visible().len();
        let current = self.selection.selected().unwrap_or(0);
        let code = match key.code {
            KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::SHIFT) => {
                KeyCode::Char(c.to_ascii_uppercase())
            }
            code => code,
        };
        if self.view != View::Revisions {
            if matches!(code, KeyCode::Esc | KeyCode::Left | KeyCode::Char('h')) {
                self.view = if self.view == View::Diff && self.diff_file {
                    View::Files
                } else {
                    View::Revisions
                };
                self.scroll = 0;
                return Effect::None;
            }
            if matches!(code, KeyCode::Char('q')) {
                return Effect::Quit;
            }
            if self.view == View::Files {
                let old_index = self.file_index;
                let len = self.preview.as_ref().map_or(0, |p| p.files.len());
                match code {
                    KeyCode::Down | KeyCode::Char('j') if len > 0 => {
                        self.file_index = (self.file_index + 1).min(len - 1)
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.file_index = self.file_index.saturating_sub(1)
                    }
                    KeyCode::PageDown if len > 0 => {
                        self.file_index = (self.file_index + page_size).min(len - 1)
                    }
                    KeyCode::PageUp => self.file_index = self.file_index.saturating_sub(page_size),
                    KeyCode::Char('d') => {
                        self.view = View::Diff;
                        self.diff_file = true;
                        self.scroll = 0;
                    }
                    KeyCode::Char('p') => self.preview_visible = !self.preview_visible,
                    _ => {}
                }
                if old_index != self.file_index {
                    self.scroll = 0;
                }
                return Effect::None;
            }
            match code {
                KeyCode::Down | KeyCode::Char('j') => self.scroll = self.scroll.saturating_add(1),
                KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::PageDown => self.scroll = self.scroll.saturating_add(page_size as u16),
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(page_size as u16),
                KeyCode::Home => self.scroll = 0,
                KeyCode::End => self.scroll = u16::MAX,
                _ => {}
            }
            return Effect::None;
        }
        if self.preview_focus {
            match code {
                KeyCode::Down | KeyCode::Char('j') => {
                    self.scroll = self.scroll.saturating_add(1);
                    return Effect::None;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.scroll = self.scroll.saturating_sub(1);
                    return Effect::None;
                }
                _ => {}
            }
        }
        match code {
            KeyCode::Char('l') | KeyCode::Right => {
                self.view = View::Files;
                self.file_index = 0;
                self.scroll = 0;
            }
            KeyCode::Char('d') => {
                self.view = View::Diff;
                self.diff_file = false;
                self.scroll = 0;
            }
            KeyCode::Char('p') => {
                self.preview_visible = !self.preview_visible;
                self.preview_focus = false;
            }
            KeyCode::Char('P') => {
                self.preview_visible = true;
                self.preview_bottom = !self.preview_bottom;
            }
            KeyCode::Tab if self.preview_visible => self.preview_focus = !self.preview_focus,
            KeyCode::Char('b') => {
                self.view = View::Bookmarks;
                self.scroll = 0;
            }
            KeyCode::Char('?') | KeyCode::F(1) => {
                self.view = View::Help;
                self.scroll = 0;
            }
            KeyCode::Char(' ') => {
                if let Some(id) = self.selected_id().map(str::to_owned)
                    && !self.marked.remove(&id)
                {
                    self.marked.insert(id);
                }
            }
            KeyCode::Char('q') => return Effect::Quit,
            KeyCode::Char('/') => {
                self.searching = true;
                self.notice = None;
            }
            KeyCode::Char('\'') => self.search_match(false, false),
            KeyCode::Char('"') => self.search_match(true, false),
            KeyCode::Esc => {
                self.filter.clear();
                self.notice = None;
                self.marked.clear();
                self.preview_focus = false;
            }
            KeyCode::Char('j') | KeyCode::Down if len > 0 => {
                self.selection.select(Some((current + 1).min(len - 1)))
            }
            KeyCode::Char('k') | KeyCode::Up if len > 0 => {
                self.selection.select(Some(current.saturating_sub(1)))
            }
            KeyCode::PageDown if len > 0 => self
                .selection
                .select(Some((current + page_size.max(1)).min(len - 1))),
            KeyCode::PageUp if len > 0 => self
                .selection
                .select(Some(current.saturating_sub(page_size.max(1)))),
            KeyCode::Home if len > 0 => self.selection.select(Some(0)),
            KeyCode::End if len > 0 => self.selection.select(Some(len - 1)),
            KeyCode::Char('@' | 'J' | 'K') if len > 0 => {
                let revisions = &self.snapshot.as_ref().unwrap().revisions;
                let target = revisions.iter().position(|r| match code {
                    KeyCode::Char('@') => r.working_copy,
                    KeyCode::Char('J') => revisions[current].parents.contains(&r.commit_id),
                    _ => r.parents.contains(&revisions[current].commit_id),
                });
                if let Some(index) = target {
                    self.selection.select(Some(index));
                }
            }
            _ => {}
        }
        Effect::None
    }

    fn update(&mut self, snapshot: Snapshot) {
        let visible = self.visible();
        let old = self
            .selection
            .selected()
            .and_then(|i| visible.get(i))
            .and_then(|i| self.snapshot.as_ref().map(|s| &s.revisions[*i]))
            .map(|r| (r.commit_id.clone(), r.change_id.clone()));
        self.graph = crate::graph::render(&snapshot.revisions);
        self.marked
            .retain(|id| snapshot.revisions.iter().any(|r| &r.commit_id == id));
        self.snapshot = Some(snapshot);
        let visible = self.visible();
        let revisions = &self.snapshot.as_ref().unwrap().revisions;
        let selected = old
            .as_ref()
            .and_then(|(commit, change)| {
                visible
                    .iter()
                    .position(|i| revisions[*i].commit_id == *commit)
                    .or_else(|| {
                        visible
                            .iter()
                            .position(|i| revisions[*i].change_id == *change)
                    })
            })
            .or_else(|| visible.iter().position(|i| revisions[*i].working_copy))
            .unwrap_or(0);
        let selected = if self.follow_working_copy {
            self.follow_working_copy = false;
            visible
                .iter()
                .position(|i| revisions[*i].working_copy)
                .unwrap_or(selected)
        } else {
            selected
        };
        self.selection
            .select((!visible.is_empty()).then_some(selected));
        if self.view == View::InlineOperations
            && old.as_ref().map(|(id, _)| id.as_str()) != self.selected_id()
        {
            self.listing.clear();
            self.operation_graph.clear();
            self.browse_index = 0;
        }
        self.error = None;
        if self
            .preview
            .as_ref()
            .is_some_and(|p| Some(p.commit_id.as_str()) != self.selected_id())
        {
            self.preview = None;
        }
    }
}

pub fn run(live: Live, keys: crate::keybinds::Keybinds) -> Result<()> {
    let mut terminal = ratatui::init();
    crossterm::execute!(std::io::stdout(), event::EnableMouseCapture)?;
    let _mouse = MouseCapture;

    // ratatui also installs a panic hook to restore the terminal.
    let result = (|| -> Result<()> {
        let mut app = App {
            keys,
            revset: live.initial_revset.clone(),
            ..App::default()
        };
        loop {
            for update in live.updates.try_iter() {
                match update {
                    Update::OperationDiff { id, result } => {
                        app.busy = false;
                        app.apply_operation_diff(&id, result);
                    }
                    Update::Repository(snapshot) => {
                        app.update(snapshot);
                        if !app.busy
                            || matches!(app.view, View::Operations | View::InlineOperations)
                        {
                            let query = match app.view {
                                View::Bookmarks => Some(crate::browser::Query::Bookmarks),
                                View::Operations => Some(crate::browser::Query::Operations),
                                View::InlineOperations => app
                                    .selected_id()
                                    .map(|id| crate::browser::Query::CommitOperations(id.into())),
                                View::Finder => app
                                    .selected_id()
                                    .map(|id| crate::browser::Query::Files(id.into())),
                                _ => None,
                            };
                            if let Some(query) = query {
                                app.busy = live
                                    .tasks
                                    .try_send(crate::live::Request::Listing(query))
                                    .is_ok();
                            }
                        }
                    }
                    Update::Error(error) => {
                        app.error = Some(error);
                        app.busy = false;
                    }
                    Update::RevsetApplied(expression) => {
                        app.revset = expression;
                        app.busy = false;
                    }
                    Update::Listing(result) => {
                        app.busy = false;
                        match result {
                            Ok(list) => {
                                if !app.accepts_listing(&list.query) {
                                    continue;
                                }
                                app.notice = None;
                                let selected = app
                                    .listing_rows()
                                    .get(app.browse_index)
                                    .map(|r| r.id.clone());
                                app.operation_graph = crate::operations::graph(&list.rows);
                                app.listing = list.rows;
                                app.browse_index = selected
                                    .and_then(|id| {
                                        app.listing_rows().iter().position(|r| r.id == id)
                                    })
                                    .unwrap_or(0);
                            }
                            Err(error) => app.error = Some(error),
                        }
                    }
                    Update::CommandFinished { command, result } => {
                        app.busy = false;
                        app.pending_command = false;
                        match result {
                            Ok(output) => {
                                app.output = output;
                                app.follow_working_copy = ["jj new", "jj edit", "jj commit"]
                                    .iter()
                                    .any(|prefix| command.starts_with(prefix));
                                app.notice = Some(format!("{command} completed"));
                                app.error = None;
                            }
                            Err(error) => {
                                app.error = Some(error);
                            }
                        }
                        app.requested = None;
                    }
                    Update::Preview { id, result } => {
                        if app.selected_id() == Some(&id) {
                            match result {
                                Ok(preview) => {
                                    app.preview = Some(preview);
                                    app.preview_error = None;
                                }
                                Err(error) => app.preview_error = Some(error),
                            }
                        }
                    }
                    Update::Files(paths) => {
                        for path in paths {
                            app.files.retain(|p| p != &path);
                            app.files.push_front(path);
                            app.files.truncate(100);
                        }
                    }
                }
            }
            if app.view != View::OperationDiff
                && (app.preview_visible || matches!(app.view, View::Files | View::Diff))
                && let Some(id) = app.selected_id().map(str::to_owned)
                && app.requested.as_ref() != Some(&id)
            {
                app.preview = None;
                app.preview_error = None;
                app.marked_files.clear();
                app.scroll = 0;
                app.file_index = 0;
                if live.preview.try_send(id.clone()).is_ok() {
                    app.requested = Some(id);
                }
            }
            terminal.draw(|frame| draw(frame, &mut app))?;
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            let key = match event::read()? {
                Event::Key(key) => key,
                Event::Mouse(mouse) => {
                    app.mouse(mouse);
                    continue;
                }
                _ => continue,
            };
            let page_size = terminal.size()?.height.saturating_sub(6) as usize / 2;
            match app.handle_key(key, page_size) {
                Effect::Quit => break,
                Effect::Refresh => {
                    let _ = live.refresh.try_send(());
                }
                Effect::Request(request) => {
                    if let Err(error) = live.tasks.try_send(request) {
                        app.busy = false;
                        app.pending_command = false;
                        app.error = Some(error.to_string());
                    }
                }
                Effect::External(spec) => {
                    crossterm::execute!(std::io::stdout(), event::DisableMouseCapture)?;
                    ratatui::restore();
                    let result = spec
                        .process(&live.root)
                        .and_then(|mut command| Ok(command.status()?));
                    terminal = ratatui::init();
                    crossterm::execute!(std::io::stdout(), event::EnableMouseCapture)?;
                    terminal.clear()?;
                    match result {
                        Ok(status) if status.success() => {
                            app.notice = Some(format!("{} completed", spec.display()))
                        }
                        Ok(status) => {
                            app.error = Some(format!("{} failed: {status}", spec.display()))
                        }
                        Err(error) => app.error = Some(error.to_string()),
                    }
                    let _ = live.refresh.try_send(());
                    app.requested = None;
                }
                Effect::Shell(command) => {
                    crossterm::execute!(std::io::stdout(), event::DisableMouseCapture)?;
                    ratatui::restore();
                    let status = std::process::Command::new(
                        std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into()),
                    )
                    .args(["-c", &command])
                    .current_dir(&live.root)
                    .status();
                    terminal = ratatui::init();
                    crossterm::execute!(std::io::stdout(), event::EnableMouseCapture)?;
                    terminal.clear()?;
                    app.notice = Some(format!("Shell: {status:?}"));
                    let _ = live.refresh.try_send(());
                    app.requested = None;
                }
                Effect::None => {}
            }
        }
        Ok(())
    })();
    ratatui::restore();
    result
}

struct MouseCapture;
impl Drop for MouseCapture {
    fn drop(&mut self) {
        let _ = crossterm::execute!(std::io::stdout(), event::DisableMouseCapture);
    }
}

fn draw(frame: &mut ratatui::Frame, app: &mut App) {
    app.hit_rows.clear();
    app.operation_hit_rows.clear();
    draw_body(frame, app);
    if let Some(input) = &app.input {
        let area = frame.area();
        let height = area.height.min(9);
        let area =
            ratatui::layout::Rect::new(area.x, area.y + area.height - height, area.width, height);
        frame.render_widget(ratatui::widgets::Clear, area);
        let text = if input.confirm {
            format!(
                "{}\nSources: {}\nFiles: {}",
                input.text,
                input.sources.join(", "),
                input.files.join(", ")
            )
        } else {
            input.visible()
        };
        let text = format!(
            "{text}\n{} apply · {} cancel · {} newline",
            app.keys.label("input", "submit"),
            app.keys.label("input", "cancel"),
            app.keys.label("input", "newline")
        );
        frame.render_widget(
            Paragraph::new(text)
                .wrap(ratatui::widgets::Wrap { trim: false })
                .block(Block::bordered().title(input.label.clone())),
            area,
        );
    }
}

fn draw_body(frame: &mut ratatui::Frame, app: &mut App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .areas(frame.area());
    let operation = app
        .snapshot
        .as_ref()
        .map(|s| short(&s.operation))
        .unwrap_or("loading");
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " jjui-rs ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "  LIVE · op {operation} · {} selected · {} recent file events",
                app.marked.len(),
                app.files.len()
            )),
        ])),
        header,
    );
    let footer_text = app
        .error
        .as_deref()
        .or(app.notice.as_deref())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            format!(
                "{} move · {} files · {} diff · {} preview · {} help · {} quit\n{} · revset: {}",
                app.keys.label("revisions", "down"),
                app.keys.label("revisions", "details"),
                app.keys.label("revisions", "diff"),
                app.keys.label("revisions", "preview"),
                app.keys.label("global", "help"),
                app.keys.label("global", "quit"),
                if app.busy {
                    "Action running…"
                } else {
                    "LIVE"
                },
                app.revset
            )
        });
    frame.render_widget(
        Paragraph::new(footer_text).style(Style::default().fg(if app.error.is_some() {
            Color::Red
        } else {
            Color::Gray
        })),
        footer,
    );
    if app.view == View::Operations {
        draw_operations(frame, body, app);
        return;
    }
    if matches!(app.view, View::Bookmarks | View::Finder) {
        let rows = app.listing_rows();
        let items: Vec<_> = rows
            .iter()
            .map(|row| ListItem::new(row.label.clone()))
            .collect();
        let mut state = ListState::default();
        if !items.is_empty() {
            state.select(Some(app.browse_index.min(items.len() - 1)));
        }
        let title = match app.view {
            View::Operations => " Operations (latest 200) ".to_owned(),
            View::Bookmarks => " Local / remote bookmarks ".to_owned(),
            _ => format!(
                " File finder · {} filter · {} open ",
                app.keys.label("finder", "search"),
                app.keys.label("finder", "open_file")
            ),
        };
        frame.render_stateful_widget(
            List::new(items)
                .block(Block::bordered().title(title))
                .highlight_style(Style::default().bg(Color::DarkGray))
                .highlight_symbol("▸ "),
            body,
            &mut state,
        );
        return;
    }
    if matches!(
        app.view,
        View::Diff | View::OperationDiff | View::Help | View::Text | View::Git
    ) {
        let (title, text) = match app.view {
            View::Diff => (" Diff ", app.preview_text()),
            View::OperationDiff => (
                " Snapshot operation diff ",
                match app
                    .operation_diff
                    .as_ref()
                    .and_then(|diff| diff.result.as_ref())
                {
                    Some(Ok(text)) => text.clone(),
                    Some(Err(error)) => format!("Cannot load operation diff: {error}"),
                    None => "Loading operation diff…".into(),
                },
            ),
            View::Help => (
                " Key bindings ",
                format!(
                    "{}\n\nRecent working-tree events\n{}",
                    app.keys.help(),
                    app.files.iter().cloned().collect::<Vec<_>>().join("\n")
                ),
            ),
            View::Git => (
                " Git remotes ",
                format!(
                    "{} fetch · {} push · {} add remote · {} remove remote\n\n{}",
                    app.keys.label("git", "fetch"),
                    app.keys.label("git", "push"),
                    app.keys.label("git", "remote_add"),
                    app.keys.label("git", "remote_remove"),
                    app.output
                ),
            ),
            _ => (" Command output / history ", app.output.clone()),
        };
        draw_text(
            frame,
            body,
            title,
            &text,
            &mut app.scroll,
            matches!(app.view, View::Diff | View::OperationDiff),
        );
        return;
    }
    let (main, preview_area) = if app.preview_visible {
        let areas = if app.preview_bottom {
            Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)]).split(body)
        } else {
            Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).split(body)
        };
        (areas[0], Some(areas[1]))
    } else {
        (body, None)
    };
    draw_graph(frame, main, app);
    if let Some(area) = preview_area {
        let text = app.preview_text();
        draw_text(
            frame,
            area,
            if app.preview_focus {
                " Diff preview · focused "
            } else {
                " Diff preview "
            },
            &text,
            &mut app.scroll,
            true,
        );
    }
}

// Keep ID disambiguation independent of the visible graph window. jj-lib supplies
// repository-wide prefix lengths; eight characters is jj's default display width.
fn log_id(id: &str, prefix_len: usize, color: Color) -> Vec<Span<'static>> {
    let end = id.len().min(prefix_len.max(8));
    let prefix = prefix_len.min(end);
    vec![
        Span::styled(
            id[..prefix].to_owned(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            id[prefix..end].to_owned(),
            Style::default().fg(Color::DarkGray),
        ),
    ]
}

fn revision_header(
    revision: &crate::repository::Revision,
    node: &str,
    marked: bool,
) -> Line<'static> {
    let wc = revision.working_copy;
    let mut spans = vec![Span::styled(
        node.to_owned(),
        Style::default().fg(Color::Green),
    )];
    spans.extend(log_id(
        &revision.change_id,
        revision.change_prefix_len,
        if wc {
            Color::LightMagenta
        } else {
            Color::Magenta
        },
    ));
    spans.push(Span::raw(" "));
    if revision.root {
        spans.push(Span::styled("root() ", Style::default().fg(Color::Green)));
    } else {
        spans.push(Span::styled(
            if revision.author_email.is_empty() {
                "(no email set)".into()
            } else {
                revision.author_email.clone()
            },
            Style::default().fg(if revision.author_email.is_empty() {
                Color::Red
            } else {
                Color::Yellow
            }),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            revision.timestamp.clone(),
            Style::default().fg(if wc { Color::LightCyan } else { Color::Cyan }),
        ));
        spans.push(Span::raw(" "));
        if !revision.bookmarks.is_empty() {
            spans.push(Span::styled(
                format!("{} ", revision.bookmarks.join(" ")),
                Style::default().fg(if wc {
                    Color::LightMagenta
                } else {
                    Color::Magenta
                }),
            ));
        }
    }
    spans.extend(log_id(
        &revision.commit_id,
        revision.commit_prefix_len,
        if wc { Color::LightBlue } else { Color::Blue },
    ));
    if revision.root && !revision.bookmarks.is_empty() {
        spans.push(Span::styled(
            format!(" {}", revision.bookmarks.join(" ")),
            Style::default().fg(Color::Magenta),
        ));
    }
    if revision.conflict {
        spans.push(Span::styled(
            " (conflict)",
            Style::default().fg(if wc { Color::LightRed } else { Color::Red }),
        ));
    }
    if marked {
        spans.push(Span::styled(
            " [selected]",
            Style::default().fg(Color::Yellow),
        ));
    }
    Line::from(spans).style(if wc {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    })
}

fn revision_description(revision: &crate::repository::Revision, edge: &str) -> Line<'static> {
    let green = if revision.working_copy {
        Color::LightGreen
    } else {
        Color::Green
    };
    let mut spans = vec![Span::styled(
        edge.to_owned(),
        Style::default().fg(Color::Green),
    )];
    if revision.empty {
        spans.push(Span::styled("(empty) ", Style::default().fg(green)));
    }
    spans.push(
        match revision
            .description
            .lines()
            .next()
            .filter(|line| !line.is_empty())
        {
            Some(description) => Span::raw(description.to_owned()),
            None => Span::styled(
                "(no description set)",
                Style::default().fg(if revision.empty { green } else { Color::Yellow }),
            ),
        },
    );
    Line::from(spans).style(if revision.working_copy {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    })
}

fn operation_lines(
    row: &crate::browser::Row,
    graph: &crate::graph::GraphRow,
    outer: &str,
) -> Vec<Line<'static>> {
    let op = row.operation.as_ref().expect("operation row");
    let style = if op.current {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let blue = if op.current {
        Color::LightBlue
    } else {
        Color::Blue
    };
    let cyan = if op.current {
        Color::LightCyan
    } else {
        Color::Cyan
    };
    let mut spans = vec![
        Span::styled(outer.to_owned(), Style::default().fg(Color::Green)),
        Span::styled(
            graph.node.clone(),
            if op.current {
                style.fg(Color::Green)
            } else {
                Style::default()
            },
        ),
        Span::raw(" "),
        Span::styled(short(&row.id).to_owned(), Style::default().fg(blue)),
    ];
    if op.root {
        spans.push(Span::styled(" root()", Style::default().fg(Color::Green)));
        return vec![Line::from(spans).style(style)];
    }
    spans.push(Span::styled(
        format!(" {}", op.user),
        Style::default().fg(Color::Yellow),
    ));
    if let Some(workspace) = &op.workspace {
        spans.push(Span::styled(
            format!(" {workspace}@"),
            Style::default().fg(Color::Green),
        ));
    }
    let elapsed = chrono::Local::now()
        .timestamp_millis()
        .saturating_sub(op.end_millis);
    let when = if elapsed >= 0 {
        format!("{} ago", operation_duration(elapsed))
    } else {
        format!("in {}", operation_duration(elapsed.saturating_abs()))
    };
    spans.push(Span::styled(
        format!(" {when}, lasted {}", operation_duration(op.duration_millis)),
        Style::default().fg(cyan),
    ));
    let mut lines = vec![Line::from(spans).style(style)];
    lines.push(
        Line::from(vec![
            Span::styled(outer.to_owned(), Style::default().fg(Color::Green)),
            Span::raw(format!("{} ", graph.edge)),
            Span::raw(op.description.lines().next().unwrap_or_default().to_owned()),
        ])
        .style(style),
    );
    for attribute in &op.attributes {
        for text in attribute.lines() {
            lines.push(
                Line::from(vec![
                    Span::styled(outer.to_owned(), Style::default().fg(Color::Green)),
                    Span::raw(format!("{} ", graph.continuation)),
                    Span::styled(
                        text.to_owned(),
                        Style::default().fg(if op.current {
                            Color::LightMagenta
                        } else {
                            Color::Magenta
                        }),
                    ),
                ])
                .style(style),
            );
        }
    }
    if let Some(boundary) = &graph.boundary {
        lines.push(Line::from(vec![
            Span::styled(outer.to_owned(), Style::default().fg(Color::Green)),
            Span::styled(boundary.clone(), Style::default().fg(Color::DarkGray)),
        ]));
    }
    lines
}

fn operation_duration(millis: i64) -> String {
    let millis = millis.max(0);
    let (value, unit) = [
        (31_536_000_000, "year"),
        (2_592_000_000, "month"),
        (86_400_000, "day"),
        (3_600_000, "hour"),
        (60_000, "minute"),
        (1_000, "second"),
        (1, "millisecond"),
    ]
    .into_iter()
    .find(|(scale, _)| millis >= *scale)
    .map(|(scale, unit)| (millis / scale, unit))
    .unwrap_or((0, "millisecond"));
    format!("{value} {unit}{}", if value == 1 { "" } else { "s" })
}

fn draw_operations(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &mut App) {
    let mut items = Vec::new();
    let mut indices = Vec::new();
    let mut selected = None;
    for (index, (row, graph)) in app.listing.iter().zip(&app.operation_graph).enumerate() {
        if index == app.browse_index {
            selected = Some(items.len());
        }
        for line in operation_lines(row, graph, "") {
            indices.push(index);
            items.push(ListItem::new(line));
        }
    }
    if items.is_empty() {
        items.push(ListItem::new(if app.busy {
            "Loading operations…"
        } else {
            "No operations"
        }));
    }
    app.operation_selection.select(selected);
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::bordered().title(" Operations (latest 200) "))
            .highlight_style(Style::default().bg(Color::Indexed(236)))
            .highlight_symbol("▸ "),
        area,
        &mut app.operation_selection,
    );
    for (line, index) in indices
        .into_iter()
        .skip(app.operation_selection.offset())
        .take(area.height.saturating_sub(2) as usize)
        .enumerate()
    {
        app.operation_hit_rows.push((
            ratatui::layout::Rect::new(
                area.x + 1,
                area.y + 1 + line as u16,
                area.width.saturating_sub(2),
                1,
            ),
            index,
        ));
    }
}

fn draw_graph(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &mut App) {
    let mut items = Vec::new();
    let mut selected_row = None;
    let mut targets = Vec::new();
    let mut operation_rows = std::collections::HashMap::new();
    if let Some(snapshot) = &app.snapshot {
        for (i, revision) in snapshot.revisions.iter().enumerate() {
            let graph = &app.graph[i];
            let selected = app.selection.selected() == Some(i);
            let style = if app.matches(i) {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            };
            if selected {
                selected_row = Some(items.len());
            }
            targets.push((i, None));
            items.push(
                ListItem::new(revision_header(
                    revision,
                    &graph.node,
                    app.marked.contains(&revision.commit_id),
                ))
                .style(style),
            );
            if !revision.root {
                targets.push((i, None));
                items.push(ListItem::new(revision_description(revision, &graph.edge)).style(style));
            }
            if selected && app.view == View::InlineOperations {
                targets.push((i, None));
                selected_row = Some(items.len());
                items.push(ListItem::new(format!(
                    "{}  Operations for change {} (latest 200 matches)",
                    graph.continuation,
                    &revision.change_id[..revision.change_id.len().min(8)]
                )));
                if app.listing.is_empty() {
                    targets.push((i, None));
                    items.push(ListItem::new(format!(
                        "{}  {}",
                        graph.continuation,
                        if app.busy {
                            "Loading operations…"
                        } else {
                            "No related operations"
                        }
                    )));
                }
                for (index, (row, op_graph)) in
                    app.listing.iter().zip(&app.operation_graph).enumerate()
                {
                    if index == app.browse_index {
                        selected_row = Some(items.len());
                    }
                    for line in operation_lines(row, op_graph, &format!("{}  ", graph.continuation))
                    {
                        operation_rows.insert(items.len(), index);
                        targets.push((i, None));
                        items.push(ListItem::new(line));
                    }
                }
            }
            if selected && app.view == View::Files {
                let inline = |text: String| {
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            graph.continuation.clone(),
                            Style::default().fg(Color::Green),
                        ),
                        Span::raw(format!("  {text}")),
                    ]))
                };
                let title = if let Some(error) = &app.preview_error {
                    format!("Files · {error}")
                } else if let Some(preview) = &app.preview {
                    format!(
                        "Files · {}{}",
                        preview.files.len(),
                        if preview.truncated {
                            " (truncated)"
                        } else {
                            ""
                        }
                    )
                } else {
                    "Files · loading…".into()
                };
                selected_row = Some(items.len());
                targets.push((i, None));
                items.push(inline(title));
                if let Some(preview) = &app.preview {
                    if preview.files.is_empty() {
                        targets.push((i, None));
                        items.push(inline("(no file changes)".into()));
                    }
                    app.file_index = app.file_index.min(preview.files.len().saturating_sub(1));
                    for (index, file) in preview.files.iter().enumerate() {
                        if !fuzzy_match(&file.path, &app.file_filter) {
                            continue;
                        }
                        if index == app.file_index {
                            selected_row = Some(items.len());
                        }
                        targets.push((i, Some(index)));
                        items.push(inline(format!(
                            "{}  {}{}",
                            file.status,
                            file.path,
                            if app.marked_files.contains(&file.path) {
                                " [selected]"
                            } else {
                                ""
                            }
                        )));
                    }
                }
            }
            if let Some(boundary) = &graph.boundary {
                targets.push((i, None));
                items.push(ListItem::new(format!(
                    "{boundary} ancestors outside loaded history"
                )));
            }
        }
    }
    app.graph_selection.select(selected_row);
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::bordered().title(format!(" Revisions · /{} ", app.filter)))
            .highlight_style(Style::default().bg(Color::Indexed(236)))
            .highlight_symbol("▸ "),
        area,
        &mut app.graph_selection,
    );
    for (row, (revision, file)) in targets
        .into_iter()
        .skip(app.graph_selection.offset())
        .take(area.height.saturating_sub(2) as usize)
        .enumerate()
    {
        if let Some(index) = operation_rows.get(&(row + app.graph_selection.offset())) {
            app.operation_hit_rows.push((
                ratatui::layout::Rect::new(
                    area.x + 1,
                    area.y + 1 + row as u16,
                    area.width.saturating_sub(2),
                    1,
                ),
                *index,
            ));
        }
        app.hit_rows.push((
            ratatui::layout::Rect::new(
                area.x + 1,
                area.y + 1 + row as u16,
                area.width.saturating_sub(2),
                1,
            ),
            revision,
            file,
        ));
    }
}

fn draw_text(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    title: &str,
    text: &str,
    scroll: &mut u16,
    diff: bool,
) {
    let count = text.lines().count();
    *scroll = (*scroll as usize)
        .min(count.saturating_sub(area.height.saturating_sub(2) as usize))
        .min(u16::MAX as usize) as u16;
    let lines: Vec<_> = text
        .lines()
        .map(|line| {
            Line::styled(
                line.to_owned(),
                Style::default().fg(if diff {
                    if line.starts_with('+') {
                        Color::Green
                    } else if line.starts_with('-') {
                        Color::Red
                    } else if line.starts_with("@@") {
                        Color::Cyan
                    } else {
                        Color::Reset
                    }
                } else {
                    Color::Reset
                }),
            )
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((*scroll, 0))
            .block(Block::bordered().title(title)),
        area,
    );
}

fn fuzzy_match(value: &str, query: &str) -> bool {
    let value = value.to_lowercase();
    let mut chars = value.chars();
    query
        .to_lowercase()
        .chars()
        .all(|needle| chars.by_ref().any(|c| c == needle))
}

fn short(value: &str) -> &str {
    &value[..value.len().min(12)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::Revision;

    fn revision(commit: &str, change: &str) -> Revision {
        Revision {
            commit_id: commit.into(),
            change_id: change.into(),
            parents: vec![],
            description: String::new(),
            author: String::new(),
            bookmarks: vec![],
            working_copy: false,
            ..Revision::default()
        }
    }

    #[test]
    fn log_metadata_matches_jj_compact_content_and_colors() {
        let mut revision = Revision {
            change_id: "yqkuklmmzzzz".into(),
            commit_id: "03a6cacbffff".into(),
            change_prefix_len: 2,
            commit_prefix_len: 2,
            author_email: "tom@ai-nc.com".into(),
            timestamp: "2026-11-06 11:18:25".into(),
            ..Revision::default()
        };
        let line = revision_header(&revision, "○ ", false);
        assert_eq!(
            line.to_string(),
            "○ yqkuklmm tom@ai-nc.com 2026-11-06 11:18:25 03a6cacb"
        );
        for (text, color, bold) in [
            ("yq", Color::Magenta, true),
            ("kuklmm", Color::DarkGray, false),
            ("tom@ai-nc.com", Color::Yellow, false),
            ("2026-11-06 11:18:25", Color::Cyan, false),
            ("03", Color::Blue, true),
            ("a6cacb", Color::DarkGray, false),
        ] {
            let span = line.spans.iter().find(|span| span.content == text).unwrap();
            assert_eq!(span.style.fg, Some(color));
            assert_eq!(span.style.add_modifier.contains(Modifier::BOLD), bold);
        }
        revision.working_copy = true;
        revision.empty = true;
        let line = revision_header(&revision, "@ ", false);
        assert!(line.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(line.spans[1].style.fg, Some(Color::LightMagenta));
        assert_eq!(
            revision_description(&revision, "│ ").to_string(),
            "│ (empty) (no description set)"
        );
        revision.root = true;
        assert_eq!(
            revision_header(&revision, "◆ ", false).to_string(),
            "◆ yqkuklmm root() 03a6cacb"
        );
        assert_eq!(
            log_id("0123456789abcdef", 10, Color::Blue)[0].content,
            "0123456789"
        );
    }

    #[test]
    fn selection_follows_rewrites_and_survives_removal() {
        let mut app = App::default();
        app.update(Snapshot {
            operation: "1".into(),
            revisions: vec![revision("a", "x"), revision("b", "y")],
        });
        app.selection.select(Some(1));
        app.update(Snapshot {
            operation: "2".into(),
            revisions: vec![revision("c", "y"), revision("a", "x")],
        });
        assert_eq!(app.selection.selected(), Some(0));
        app.update(Snapshot {
            operation: "3".into(),
            revisions: vec![revision("a", "x")],
        });
        assert_eq!(app.selection.selected(), Some(0));
        app.filter = "no match".into();
        app.update(Snapshot {
            operation: "4".into(),
            revisions: vec![revision("a", "x")],
        });
        assert_eq!(app.selection.selected(), Some(0));
    }
    fn press(app: &mut App, code: KeyCode) -> Effect {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE), 2)
    }

    fn key_fixture() -> App {
        let mut app = App::default();
        let mut child = revision("child", "match-one");
        child.parents.push("parent".into());
        child.working_copy = true;
        app.update(Snapshot {
            operation: "op".into(),
            revisions: vec![
                child,
                revision("parent", "other"),
                revision("third", "match-two"),
            ],
        });
        app
    }

    #[test]
    fn navigation_uses_jjui_keys_and_modifiers() {
        let mut app = key_fixture();
        press(&mut app, KeyCode::Char('J'));
        assert_eq!(app.selection.selected(), Some(1));
        press(&mut app, KeyCode::Char('K'));
        assert_eq!(app.selection.selected(), Some(0));
        press(&mut app, KeyCode::PageDown);
        assert_eq!(app.selection.selected(), Some(2));
        assert!(matches!(
            press(&mut app, KeyCode::Char('g')),
            Effect::Request(_)
        ));
        assert_eq!(app.selection.selected(), Some(2));
        app.busy = false;
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('@'));
        assert_eq!(app.selection.selected(), Some(0));
        assert_eq!(press(&mut app, KeyCode::Char('r')), Effect::None);
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL), 2),
            Effect::Refresh
        );
        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL), 2);
        assert_eq!(app.selection.selected(), Some(0));
    }

    #[test]
    fn quick_search_cycles_without_hiding_revisions() {
        let mut app = key_fixture();
        press(&mut app, KeyCode::Char('/'));
        for c in "match".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.visible().len(), 3);
        assert_eq!(app.selection.selected(), Some(0));
        press(&mut app, KeyCode::Char('\''));
        assert_eq!(app.selection.selected(), Some(2));
        press(&mut app, KeyCode::Char('\''));
        assert_eq!(app.selection.selected(), Some(0));
        press(&mut app, KeyCode::Char('"'));
        assert_eq!(app.selection.selected(), Some(2));
        press(&mut app, KeyCode::Esc);
        assert!(app.filter.is_empty());
        assert_eq!(app.selection.selected(), Some(2));
    }

    #[test]
    fn empty_repository_navigation_is_safe() {
        let mut app = App::default();
        for code in [
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::Char('@'),
            KeyCode::Char('J'),
            KeyCode::Char('K'),
        ] {
            press(&mut app, code);
        }
        assert_eq!(app.selection.selected(), None);
    }
    #[test]
    fn details_diff_preview_and_selection_bindings() {
        let mut app = key_fixture();
        app.preview = Some(crate::preview::Preview {
            commit_id: "child".into(),
            truncated: false,
            files: vec![crate::preview::FileDiff {
                path: "file".into(),
                status: "A".into(),
                patch: String::new(),
            }],
        });
        press(&mut app, KeyCode::Char(' '));
        assert!(app.marked.contains("child"));
        press(&mut app, KeyCode::Char('p'));
        assert!(app.preview_visible);
        press(&mut app, KeyCode::Char('P'));
        assert!(app.preview_bottom);
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.view, View::Files);
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.view, View::Diff);
        assert!(app.diff_file);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.view, View::Files);
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(app.view, View::Revisions);
        press(&mut app, KeyCode::Esc);
        assert!(app.marked.is_empty());
        press(&mut app, KeyCode::Char('d'));
        assert!(!app.diff_file);
    }

    #[test]
    fn graph_and_diff_render_in_small_and_normal_terminals() {
        let mut app = key_fixture();
        app.preview = Some(crate::preview::Preview {
            commit_id: "child".into(),
            truncated: false,
            files: vec![crate::preview::FileDiff {
                path: "file.txt".into(),
                status: "M".into(),
                patch: "-old\n+new\n".into(),
            }],
        });
        for (width, height) in [(120, 35), (20, 5), (1, 1)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            for view in [
                View::Revisions,
                View::Files,
                View::Diff,
                View::Help,
                View::Bookmarks,
            ] {
                app.view = view;
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            }
        }
    }
    fn rendered_rows(app: &mut App, height: u16) -> Vec<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .chunks(120)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect()
    }

    fn operation_fixture(count: usize) -> Vec<crate::browser::Row> {
        (0..count)
            .map(|i| crate::browser::Row {
                id: format!("{i:012x}"),
                label: format!("operation-{i}"),
                operation: Some(crate::operations::Entry {
                    current: i == 0,
                    parents: if i + 1 < count {
                        vec![format!("{:012x}", i + 1)]
                    } else {
                        vec![]
                    },
                    user: "tom@host".into(),
                    workspace: Some("default".into()),
                    end_millis: chrono::Local::now().timestamp_millis() - 60000,
                    duration_millis: 7,
                    description: format!("operation-{i}"),
                    attributes: vec!["args: jj describe".into()],
                    ..Default::default()
                }),
            })
            .collect()
    }

    #[test]
    fn operations_expand_inline_switch_to_full_and_preserve_selection() {
        let mut app = key_fixture();
        let before = rendered_rows(&mut app, 35);
        let parent_before = before
            .iter()
            .position(|line| line.contains("other") && line.contains("parent"))
            .unwrap();
        assert_eq!(
            press(&mut app, KeyCode::Char('o')),
            Effect::Request(crate::live::Request::Listing(
                crate::browser::Query::CommitOperations("child".into())
            ))
        );
        assert_eq!(app.view, View::InlineOperations);
        assert!(!app.accepts_listing(&crate::browser::Query::Operations));
        assert!(!app.accepts_listing(&crate::browser::Query::CommitOperations("parent".into())));
        assert!(app.accepts_listing(&crate::browser::Query::CommitOperations("child".into())));
        app.listing = operation_fixture(2);
        app.operation_graph = crate::operations::graph(&app.listing);
        app.busy = false;
        let expanded = rendered_rows(&mut app, 35);
        let parent_after = expanded
            .iter()
            .position(|line| line.contains("other") && line.contains("parent"))
            .unwrap();
        let operation = expanded
            .iter()
            .position(|line| line.contains("operation-0"))
            .unwrap();
        assert!(operation < parent_after);
        assert_eq!(parent_after, parent_before + 7);
        assert!(expanded[operation].contains("│   │"));
        press(&mut app, KeyCode::Down);
        assert_eq!(app.browse_index, 1);
        assert_eq!(app.selection.selected(), Some(0));
        press(&mut app, KeyCode::Char('r'));
        assert_eq!(app.input.as_ref().unwrap().text, "000000000001");
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.view, View::Revisions);
        assert_eq!(rendered_rows(&mut app, 35), before);
        assert_eq!(
            press(&mut app, KeyCode::Char('O')),
            Effect::Request(crate::live::Request::Listing(
                crate::browser::Query::Operations
            ))
        );
        assert_eq!(app.view, View::Operations);
        assert!(!app.accepts_listing(&crate::browser::Query::CommitOperations("child".into())));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.selection.selected(), Some(0));
    }

    #[test]
    fn operation_graph_colors_and_scroll_work_in_both_modes() {
        let mut app = key_fixture();
        app.listing = operation_fixture(40);
        app.operation_graph = crate::operations::graph(&app.listing);
        let lines = operation_lines(&app.listing[0], &app.operation_graph[0], "");
        assert!(
            lines[0]
                .to_string()
                .starts_with("@  000000000000 tom@host default@ ")
        );
        assert!(lines[0].to_string().ends_with(", lasted 7 milliseconds"));
        for (text, color) in [
            ("000000000000", Color::LightBlue),
            (" tom@host", Color::Yellow),
            (" default@", Color::Green),
        ] {
            assert_eq!(
                lines[0]
                    .spans
                    .iter()
                    .find(|span| span.content == text)
                    .unwrap()
                    .style
                    .fg,
                Some(color)
            );
        }
        assert!(lines[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(
            lines[2].spans.last().unwrap().style.fg,
            Some(Color::LightMagenta)
        );
        for view in [View::Operations, View::InlineOperations] {
            app.view = view;
            app.browse_index = 0;
            press(&mut app, KeyCode::End);
            assert_eq!(app.browse_index, 39);
            let lines = rendered_rows(&mut app, 14);
            assert!(
                lines
                    .iter()
                    .any(|line| line.contains("000000000027") && line.contains('▸'))
            );
            press(&mut app, KeyCode::Home);
            let lines = rendered_rows(&mut app, 14);
            assert!(
                lines
                    .iter()
                    .any(|line| line.contains("000000000000") && line.contains('▸'))
            );
        }
    }

    #[test]
    fn snapshot_diff_binding_returns_to_both_operation_views_and_rejects_stale_results() {
        for origin in [View::Operations, View::InlineOperations] {
            let mut app = key_fixture();
            app.view = origin;
            app.listing = operation_fixture(3);
            app.operation_graph = crate::operations::graph(&app.listing);
            app.browse_index = 1;
            assert_eq!(press(&mut app, KeyCode::Char('d')), Effect::None);
            assert_eq!(app.view, origin);
            app.listing[1].operation.as_mut().unwrap().snapshot = true;
            app.keys.merge("[operations]\ndiff=['D']").unwrap();
            assert_eq!(press(&mut app, KeyCode::Char('d')), Effect::None);
            assert_eq!(
                press(&mut app, KeyCode::Char('D')),
                Effect::Request(crate::live::Request::OperationDiff("000000000001".into()))
            );
            assert_eq!(app.view, View::OperationDiff);
            app.apply_operation_diff("000000000002", Ok("wrong operation".into()));
            assert!(app.operation_diff.as_ref().unwrap().result.is_none());
            app.apply_operation_diff(
                "000000000001",
                Ok("--- file\n+++ file\n-before\n+after".into()),
            );
            let rows = rendered_rows(&mut app, 20);
            assert!(
                rows.iter()
                    .any(|line| line.contains("Snapshot operation diff"))
            );
            assert!(rows.iter().any(|line| line.contains("+after")));
            assert!(matches!(
                press(&mut app, KeyCode::Esc),
                Effect::Request(crate::live::Request::Listing(_))
            ));
            assert_eq!(app.view, origin);
            assert_eq!(app.browse_index, 1);
            app.apply_operation_diff("000000000001", Err("stale error".into()));
            assert!(app.operation_diff.is_none());
        }
    }

    #[test]
    fn expanded_files_push_graph_down_and_preserve_edges() {
        let mut app = key_fixture();
        app.preview = Some(crate::preview::Preview {
            commit_id: "child".into(),
            truncated: false,
            files: vec![crate::preview::FileDiff {
                path: "changed.txt".into(),
                status: "M".into(),
                patch: "patch".into(),
            }],
        });
        let before = rendered_rows(&mut app, 30);
        let parent_before = before
            .iter()
            .position(|line| line.contains("other") && line.contains("parent"))
            .unwrap();
        press(&mut app, KeyCode::Char('l'));
        let expanded = rendered_rows(&mut app, 30);
        let file_row = expanded
            .iter()
            .position(|line| line.contains("changed.txt"))
            .unwrap();
        let parent_after = expanded
            .iter()
            .position(|line| line.contains("other") && line.contains("parent"))
            .unwrap();
        let child_row = expanded
            .iter()
            .position(|line| line.contains("match-on") && line.contains("child"))
            .unwrap();
        assert!(child_row < file_row && file_row < parent_after);
        assert_eq!(parent_after, parent_before + 2);
        assert!(expanded[file_row].contains("│   M  changed.txt"));
        assert!(expanded[file_row].contains('▸'));
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.view, View::Files);
        assert!(
            rendered_rows(&mut app, 30)
                .iter()
                .any(|line| line.contains("changed.txt"))
        );
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(rendered_rows(&mut app, 30), before);
        assert_eq!(app.selection.selected(), Some(0));
    }

    #[test]
    fn long_inline_file_list_scrolls_to_selected_file_and_collapses() {
        let mut app = key_fixture();
        app.preview = Some(crate::preview::Preview {
            commit_id: "child".into(),
            truncated: false,
            files: (0..40)
                .map(|i| crate::preview::FileDiff {
                    path: format!("file-{i:02}.txt"),
                    status: "A".into(),
                    patch: String::new(),
                })
                .collect(),
        });
        press(&mut app, KeyCode::Right);
        for _ in 0..39 {
            press(&mut app, KeyCode::Down);
        }
        assert!(
            rendered_rows(&mut app, 12)
                .iter()
                .any(|line| line.contains("file-39.txt") && line.contains('▸'))
        );
        press(&mut app, KeyCode::Left);
        let collapsed = rendered_rows(&mut app, 12);
        assert!(
            collapsed.iter().any(|line| line.contains("match-on")
                && line.contains("child")
                && line.contains('▸'))
        );
        assert!(!collapsed.iter().any(|line| line.contains("file-")));
    }
    #[test]
    fn custom_bindings_drive_dispatch_and_input_target_submission() {
        let mut app = key_fixture();
        app.keys
            .merge("[revisions]\ndown=['ctrl+j']\nnew=[]\n[input]\nsubmit=['ctrl+enter']\n")
            .unwrap();
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selection.selected(), Some(0));
        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL), 2);
        assert_eq!(app.selection.selected(), Some(1));
        assert_eq!(press(&mut app, KeyCode::Char('n')), Effect::None);
        press(&mut app, KeyCode::Char('r'));
        assert!(app.target.is_some());
        press(&mut app, KeyCode::Home);
        let effect = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL), 2);
        let Effect::Request(crate::live::Request::Command(spec)) = effect else {
            panic!("expected rebase")
        };
        assert_eq!(spec.args, vec!["rebase", "-r", "other", "-o", "match-one"]);
    }
    #[test]
    fn input_characters_do_not_trigger_actions_and_cancel_has_no_mutation() {
        let mut app = key_fixture();
        press(&mut app, KeyCode::Enter);
        for c in "qnr😀".chars() {
            assert_eq!(press(&mut app, KeyCode::Char(c)), Effect::None);
        }
        assert_eq!(app.input.as_ref().unwrap().text, "qnr😀");
        press(&mut app, KeyCode::Esc);
        assert!(app.input.is_none());
        assert!(!app.busy);
        press(&mut app, KeyCode::Char('a'));
        assert!(app.input.as_ref().unwrap().confirm);
        press(&mut app, KeyCode::Esc);
        assert!(!app.busy);
    }
    #[test]
    fn mouse_targets_follow_inline_files_and_graph_scroll() {
        let mut app = key_fixture();
        rendered_rows(&mut app, 30);
        let (rect, _, _) = app
            .hit_rows
            .iter()
            .find(|(_, i, _)| *i == 1)
            .copied()
            .unwrap();
        app.mouse(event::MouseEvent {
            kind: event::MouseEventKind::Down(event::MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.selection.selected(), Some(1));
    }
    #[test]
    fn empty_file_filter_cannot_trigger_whole_revision_restore() {
        let mut app = key_fixture();
        app.view = View::Files;
        press(&mut app, KeyCode::Char('r'));
        assert!(app.input.is_none());
        assert!(!app.pending_command);
        assert!(app.notice.as_ref().unwrap().contains("No matching files"));
        press(&mut app, KeyCode::F(1));
        assert_eq!(app.view, View::Help);
    }
}
