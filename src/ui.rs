mod version;
mod workspaces;
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
    jj_version: version::Status,
    workspace_menu: Option<workspaces::Menu>,
    full_snapshot: Option<Snapshot>,
    expanded_immutable: HashSet<String>,
    ancestors_expanded: bool,
    hidden_immutable: std::collections::HashMap<String, Vec<String>>,
    keys: crate::keybinds::Keybinds,
    repository_path: String,
    plugin_session: Option<crate::plugins::Session>,
    plugin_call: Option<crate::plugins::Call>,
    plugin_computing: bool,
    plugin_actions: VecDeque<crate::plugins::Action>,
    plugin_choice: Option<crate::plugins::Choice>,
    output_is_diff: bool,
    theme: crate::theme::Theme,
    chunks: Option<ChunkEditor>,
    pending_chunk: Option<crate::chunks::Request>,
    key_sequence: Vec<KeyEvent>,
    key_sequence_scope: String,
    search_backwards: bool,
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
    chunk_hit_rows: Vec<(ratatui::layout::Rect, usize, usize)>,
    browse_index: usize,
    bookmark_completion: Option<(String, usize)>,
    bookmark_category: String,
    bookmark_filter: String,
    bookmark_remotes: Vec<String>,
    bookmark_remote: usize,
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
    Chunks,
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

struct ChunkEditor {
    line_offset: usize,
    id: String,
    path: String,
    session: Option<crate::chunks::Session>,
    error: Option<String>,
    index: usize,
    selected: std::collections::BTreeSet<usize>,
}

struct OperationDiff {
    id: String,
    origin: View,
    result: Option<Result<String, String>>,
}

#[derive(Debug, PartialEq)]
enum Effect {
    SwitchWorkspace(std::path::PathBuf),
    ReloadTheme,
    None,
    Quit,
    Refresh,
    Request(crate::live::Request),
    External(crate::commands::CommandSpec),
    Shell(String),
}

impl App {
    fn selected_elision(&self) -> Option<&Vec<String>> {
        let revision = self
            .snapshot
            .as_ref()?
            .revisions
            .get(self.selection.selected()?)?;
        self.hidden_immutable.get(&revision.commit_id)
    }
    fn expand_immutable(&mut self) {
        if let Some(ids) = self.selected_elision().cloned() {
            let ancestor_row = self
                .snapshot
                .as_ref()
                .and_then(|s| self.selection.selected().and_then(|i| s.revisions.get(i)))
                .is_some_and(|r| r.outside_revset);
            if ancestor_row {
                self.ancestors_expanded = true;
            } else {
                self.expanded_immutable = ids.into_iter().collect();
            }
            if let Some(snapshot) = self.full_snapshot.clone() {
                self.update(snapshot);
            }
            self.view = View::Revisions;
            self.notice = None;
        }
    }
    fn collapse_immutable(&mut self) {
        if self.expanded_immutable.is_empty() && !self.ancestors_expanded {
            return;
        }
        let focused = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| {
                self.selection
                    .selected()
                    .and_then(|index| snapshot.revisions.get(index))
            })
            .map(|r| r.commit_id.clone());
        if self.expanded_immutable.is_empty() {
            self.ancestors_expanded = false;
        } else {
            self.expanded_immutable.clear();
        }
        if let Some(snapshot) = self.full_snapshot.clone() {
            self.update(snapshot);
        }
        if let Some(focused) = focused
            && let Some((head, _)) = self
                .hidden_immutable
                .iter()
                .find(|(_, ids)| ids.contains(&focused))
        {
            let index = self
                .snapshot
                .as_ref()
                .unwrap()
                .revisions
                .iter()
                .position(|r| &r.commit_id == head);
            self.selection.select(index);
        }
        self.notice = None;
    }
    fn collapse_unfocused_immutable(&mut self) {
        let focused = self
            .snapshot
            .as_ref()
            .and_then(|s| self.selection.selected().and_then(|i| s.revisions.get(i)));
        if self.ancestors_expanded
            && !focused.is_some_and(|r| {
                self.full_snapshot
                    .as_ref()
                    .is_some_and(|s| s.ancestors.iter().any(|a| a.commit_id == r.commit_id))
            })
        {
            self.expanded_immutable.clear();
            self.collapse_immutable();
            return;
        }
        if !self.expanded_immutable.is_empty()
            && !self
                .selected_id()
                .is_some_and(|id| self.expanded_immutable.contains(id))
        {
            self.collapse_immutable();
        }
    }
    fn start_plugin(&mut self, name: &str) {
        if self.selected_elision().is_some() {
            self.notice = Some("Expand immutable revisions first".into());
            return;
        }
        if self.plugin_session.is_some() || self.pending_command {
            self.notice = Some("An action is still running".into());
            return;
        }
        let revision = self.snapshot.as_ref().and_then(|snapshot| {
            self.selection
                .selected()
                .and_then(|index| snapshot.revisions.get(index))
        });
        let context = crate::plugins::Context {
            repository: self.repository_path.clone(),
            change: revision.map(|r| r.change_id.clone()).unwrap_or_default(),
            commit: revision.map(|r| r.commit_id.clone()).unwrap_or_default(),
            file: if matches!(self.view, View::Files | View::Chunks)
                || (self.view == View::Diff && self.diff_file)
            {
                self.preview
                    .as_ref()
                    .and_then(|p| p.files.get(self.file_index))
                    .map(|f| f.path.clone())
                    .unwrap_or_default()
            } else {
                String::new()
            },
            changes: self.sources(),
            files: self.selected_files(),
            revset: self.revset.clone(),
        };
        self.plugin_session = Some(crate::plugins::Session {
            name: name.into(),
            context: context.clone(),
            callback: None,
        });
        self.plugin_call = Some(crate::plugins::Call {
            name: name.into(),
            function: String::new(),
            context,
            response: Default::default(),
        });
        self.error = None;
        self.notice = Some(format!("Running {name}…"));
    }
    fn plugin_error(&mut self, error: String) {
        self.plugin_computing = false;
        self.plugin_session = None;
        self.plugin_actions.clear();
        self.plugin_choice = None;
        self.plugin_call = None;
        self.error = Some(format!("Plugin: {error}"));
        self.history.push(format!("Plugin error: {error}"));
        if self.history.len() > 100 {
            self.history.remove(0);
        }
        self.output = error;
        self.output_is_diff = false;
    }
    fn resume_plugin(&mut self, response: crate::plugins::Response) {
        let Some(session) = &mut self.plugin_session else {
            return;
        };
        let Some(function) = session.callback.take() else {
            return;
        };
        if function.is_empty() {
            if !response.error.is_empty() {
                self.plugin_error(response.error);
            } else {
                self.notice = Some(format!("{} completed", session.name));
                self.plugin_session = None;
            }
        } else {
            self.plugin_call = Some(crate::plugins::Call {
                name: session.name.clone(),
                function,
                context: session.context.clone(),
                response,
            });
        }
    }
    fn next_plugin_effect(&mut self) -> Option<Effect> {
        use crate::plugins::{Action, Task};
        while let Some(action) = self.plugin_actions.pop_front() {
            if let Some(callback) = action.callback()
                && let Some(session) = &mut self.plugin_session
            {
                session.callback = Some(callback.into());
            }
            let task = match action {
                Action::Notify(text) => {
                    self.notice = Some(text);
                    continue;
                }
                Action::Diff(text) => {
                    self.output = text;
                    self.output_is_diff = true;
                    self.view = View::Text;
                    self.scroll = 0;
                    continue;
                }
                Action::Select(id) => {
                    if let Some(index) = self.snapshot.as_ref().and_then(|snapshot| {
                        snapshot
                            .revisions
                            .iter()
                            .position(|r| r.commit_id == id || r.change_id == id)
                    }) {
                        self.selection.select(Some(index));
                        self.view = View::Revisions;
                    } else {
                        self.plugin_error(format!("Revision {id} is not in the current graph"));
                        return None;
                    }
                    continue;
                }
                Action::Prompt { title, .. } => {
                    self.start_input("plugin_prompt", &title, String::new(), false);
                    return None;
                }
                Action::Choose { title, options, .. } => {
                    self.plugin_choice = Some(crate::plugins::Choice {
                        title,
                        options,
                        index: 0,
                    });
                    return None;
                }
                Action::ReadDiff { commit, .. } => Task::ReadDiff(commit),
                Action::Command { args, .. } => {
                    let spec = crate::commands::CommandSpec::new(args.clone());
                    self.history.push(spec.display());
                    if self.history.len() > 100 {
                        self.history.remove(0);
                    }
                    Task::Command(args)
                }
                Action::Revset { expression, .. } => Task::Revset(expression),
                Action::Refresh { .. } => Task::Refresh,
            };
            self.busy = true;
            self.pending_command = true;
            return Some(Effect::Request(crate::live::Request::Plugin(task)));
        }
        if !self.plugin_computing
            && self.plugin_call.is_none()
            && self
                .plugin_session
                .as_ref()
                .is_some_and(|session| session.callback.is_none())
        {
            if let Some(session) = &self.plugin_session
                && self.notice.as_deref() == Some(&format!("Running {}…", session.name))
            {
                self.notice = Some(format!("{} completed", session.name));
            }
            self.plugin_session = None;
        }
        None
    }
    fn selected_id(&self) -> Option<&str> {
        self.snapshot
            .as_ref()?
            .revisions
            .get(self.selection.selected()?)
            .filter(|r| !self.hidden_immutable.contains_key(&r.commit_id))
            .map(|r| r.commit_id.as_str())
    }

    fn preview_text(&self) -> String {
        if self.selected_elision().is_some() {
            return "Expand immutable revisions to view a diff".into();
        }
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
            "{} {} {} {} {}",
            r.change_id,
            r.commit_id,
            r.description,
            r.bookmarks.join(" "),
            r.tags.join(" ")
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
    fn binding_action(&mut self, scope: &str, key: KeyEvent) -> Option<String> {
        if self.key_sequence_scope != scope {
            self.key_sequence.clear();
            self.key_sequence_scope = scope.into();
        }
        let pending = !self.key_sequence.is_empty();
        if pending
            && self
                .keys
                .resolve(scope, key)
                .as_deref()
                .is_some_and(|action| matches!(action, "cancel" | "quit"))
        {
            self.key_sequence.clear();
            self.notice = None;
            if self.keys.resolve(scope, key).as_deref() == Some("cancel") {
                return None;
            }
        }
        self.key_sequence.push(key);
        let action = match self.keys.resolve_sequence(scope, &self.key_sequence) {
            crate::keybinds::BindingMatch::Prefix => {
                self.notice = Some("Key sequence…".into());
                return None;
            }
            crate::keybinds::BindingMatch::Action(action) => action,
            crate::keybinds::BindingMatch::None => {
                self.key_sequence.clear();
                if pending {
                    self.notice = Some("Unknown key sequence".into());
                }
                return None;
            }
        };
        self.key_sequence.clear();
        if pending {
            self.notice = None;
        }
        Some(action)
    }
    fn handle_key(&mut self, key: KeyEvent, page_size: usize) -> Effect {
        if key.kind != KeyEventKind::Release
            && self.view == View::Revisions
            && self.input.is_none()
            && self.target.is_none()
            && self.workspace_menu.is_none()
            && self.plugin_choice.is_none()
            && self.key_sequence.is_empty()
            && (!self.expanded_immutable.is_empty() || self.ancestors_expanded)
            && self.keys.resolve("revisions", key).as_deref() == Some("cancel")
        {
            self.collapse_immutable();
            return Effect::None;
        }
        let effect = self.handle_key_inner(key, page_size);
        self.collapse_unfocused_immutable();
        effect
    }
    fn handle_key_inner(&mut self, key: KeyEvent, page_size: usize) -> Effect {
        if key.kind == KeyEventKind::Release {
            return Effect::None;
        }
        if let Some(choice) = &mut self.plugin_choice {
            match self.keys.resolve("choice", key).as_deref() {
                Some("down") => {
                    choice.index = (choice.index + 1).min(choice.options.len().saturating_sub(1))
                }
                Some("up") => choice.index = choice.index.saturating_sub(1),
                Some("first") => choice.index = 0,
                Some("last") => choice.index = choice.options.len().saturating_sub(1),
                Some("submit") => {
                    let value = choice.options[choice.index].clone();
                    self.plugin_choice = None;
                    self.resume_plugin(crate::plugins::Response {
                        value,
                        ..Default::default()
                    });
                }
                Some("cancel") => {
                    self.plugin_choice = None;
                    self.resume_plugin(crate::plugins::Response {
                        cancelled: true,
                        ..Default::default()
                    });
                }
                Some("quit") => return Effect::Quit,
                _ => {}
            }
            return Effect::None;
        }
        if self.workspace_menu.is_some() && self.input.is_none() {
            return self.workspace_key(key, page_size);
        }
        if self.input.is_some() {
            self.key_sequence.clear();
            let action = self.keys.resolve("input", key);
            if self
                .input
                .as_ref()
                .is_some_and(|input| input.action == "set_bookmark")
                && matches!(action.as_deref(), Some("complete" | "complete_back"))
            {
                let input = self.input.as_ref().unwrap();
                let prefix = self
                    .bookmark_completion
                    .as_ref()
                    .map(|(prefix, _)| prefix.as_str())
                    .unwrap_or(&input.text);
                let mut suggestions: Vec<_> = self
                    .listing
                    .iter()
                    .filter_map(|row| row.bookmark.as_ref())
                    .filter(|item| {
                        item.kind == "move"
                            && !item.args.iter().any(|arg| arg == "--allow-backwards")
                            && item.name.starts_with(prefix)
                    })
                    .map(|item| item.name.clone())
                    .collect();
                suggestions.sort();
                suggestions.dedup();
                if !suggestions.is_empty() {
                    let reverse = action.as_deref() == Some("complete_back");
                    let index = self
                        .bookmark_completion
                        .as_ref()
                        .map(|(_, index)| {
                            (index + if reverse { suggestions.len() - 1 } else { 1 })
                                % suggestions.len()
                        })
                        .unwrap_or(if reverse { suggestions.len() - 1 } else { 0 });
                    self.bookmark_completion = Some((prefix.to_owned(), index));
                    let input = self.input.as_mut().unwrap();
                    input.text = suggestions[index].clone();
                    input.cursor = input.text.len();
                }
                return Effect::None;
            }
            self.bookmark_completion = None;
            if action.as_deref() == Some("cancel") {
                if self
                    .input
                    .as_ref()
                    .is_some_and(|input| input.action == "plugin_prompt")
                {
                    self.input = None;
                    self.resume_plugin(crate::plugins::Response {
                        cancelled: true,
                        ..Default::default()
                    });
                    return Effect::None;
                }
                if self
                    .input
                    .as_ref()
                    .is_some_and(|input| input.action == "bookmark_filter")
                {
                    self.bookmark_filter.clear();
                    self.browse_index = 0;
                }
                self.input = None;
                if let Some(menu) = &mut self.workspace_menu {
                    menu.pending = None;
                }
                self.pending_chunk = None;
                return Effect::None;
            }
            if matches!(action.as_deref(), Some("submit" | "accept")) {
                let input = self.input.take().unwrap();
                if input.action.starts_with("workspace_") {
                    return self.workspace_submit(input);
                }
                match input.action.as_str() {
                    "plugin_prompt" => {
                        self.resume_plugin(crate::plugins::Response {
                            value: input.text,
                            ..Default::default()
                        });
                        return Effect::None;
                    }
                    "chunk_restore" | "chunk_split" => {
                        if let Some(mut request) = self.pending_chunk.take() {
                            request.description = input.text;
                            self.pending_command = true;
                            self.busy = true;
                            return Effect::Request(crate::live::Request::ApplyChunks(request));
                        }
                        return Effect::None;
                    }
                    "revset" => {
                        self.busy = true;
                        return Effect::Request(crate::live::Request::Revset(input.text));
                    }
                    "command" if matches!(input.text.trim(), "q" | "q!" | "quit" | "quit!") => {
                        return Effect::Quit;
                    }
                    "command" if matches!(input.text.trim(), "help" | "h") => {
                        self.view = View::Help;
                        self.scroll = 0;
                        return Effect::None;
                    }
                    "search" | "search_backward" => {
                        self.search_backwards = input.action == "search_backward";
                        self.filter = input.text;
                        self.search_match(self.search_backwards, true);
                        return Effect::None;
                    }
                    "bookmark_filter" => {
                        self.bookmark_filter = input.text;
                        self.browse_index = 0;
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
                        | "input_end" | "newline" | "delete_word" | "delete_to_start",
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
                if input.action == "set_bookmark" {
                    input.text = input.text.replace(' ', "-");
                }
                if matches!(input.action.as_str(), "search" | "search_backward") {
                    self.search_backwards = input.action == "search_backward";
                    self.filter = input.text.clone();
                    self.search_match(self.search_backwards, true);
                } else if input.action == "bookmark_filter" {
                    self.bookmark_filter = input.text.clone();
                    self.browse_index = 0;
                } else if input.action == "file_filter" {
                    self.file_filter = input.text.clone();
                    self.browse_index = 0;
                }
            }
            return Effect::None;
        }
        if self.target.is_some()
            && self.selected_elision().is_none()
            && self.keys.resolve("input", key).as_deref() == Some("submit")
        {
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
        let scope = if self.selected_elision().is_some()
            && matches!(
                self.view,
                View::Revisions | View::InlineOperations | View::Files
            ) {
            "elision"
        } else {
            match self.view {
                View::Revisions => "revisions",
                View::Files => "files",
                View::Chunks => "chunks",
                View::Bookmarks => "bookmarks",
                View::Operations | View::InlineOperations => "operations",
                View::Git => "git",
                View::Finder => "finder",
                _ => "view",
            }
        };
        let Some(mut action) = self.binding_action(scope, key) else {
            return Effect::None;
        };
        if action == "expand" {
            self.expand_immutable();
            return Effect::None;
        }
        if scope == "elision" {
            self.view = View::Revisions;
        }
        // The event loop supplies half a screen; full-page motions use twice that.
        let page_size = if matches!(action.as_str(), "page_down" | "page_up") {
            page_size.saturating_mul(2)
        } else {
            page_size
        };
        action = match action.as_str() {
            "half_page_down" => "page_down".into(),
            "half_page_up" => "page_up".into(),
            _ => action,
        };
        if matches!(action.as_str(), "next_match" | "previous_match") {
            self.search_match(self.search_backwards ^ (action == "previous_match"), false);
            return Effect::None;
        }
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
        if action.starts_with("plugin.") {
            self.start_plugin(&action);
            return Effect::None;
        }
        if (self.pending_command || self.plugin_session.is_some())
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
                    | "workspace_update"
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
        if matches!(self.view, View::Bookmarks | View::Git) {
            match action.as_str() {
                "bookmark_apply" => return self.apply_bookmark(),
                "fetch" | "push" if self.view == View::Git => {
                    if self.bookmark_category == action {
                        return self.apply_bookmark();
                    }
                    self.bookmark_category = action.clone();
                    self.browse_index = 0;
                    return Effect::None;
                }
                "bookmark_move" | "bookmark_delete" | "bookmark_forget" | "bookmark_track"
                | "bookmark_untrack" => {
                    let kind = action.strip_prefix("bookmark_").unwrap();
                    if self.bookmark_category == kind {
                        if let Some(index) = self.listing_rows().iter().position(|row| {
                            row.bookmark.as_ref().is_some_and(|action| {
                                action.kind == "move"
                                    && matches!(action.name.as_str(), "main" | "master")
                            })
                        }) {
                            self.browse_index = index;
                        }
                        return self.apply_bookmark();
                    }
                    self.bookmark_category = kind.into();
                    self.browse_index = 0;
                    return Effect::None;
                }
                "bookmark_next_remote" | "bookmark_previous_remote" => {
                    let count = self.bookmark_remotes.len();
                    if count > 0 {
                        self.bookmark_remote = (self.bookmark_remote
                            + if action == "bookmark_next_remote" {
                                1
                            } else {
                                count - 1
                            })
                            % count;
                    }
                    self.browse_index = 0;
                    return Effect::None;
                }
                "search" => {
                    self.start_input(
                        "bookmark_filter",
                        if self.view == View::Git {
                            "Filter Git operations"
                        } else {
                            "Filter bookmark operations"
                        },
                        self.bookmark_filter.clone(),
                        false,
                    );
                    return Effect::None;
                }
                "cancel"
                    if !self.bookmark_category.is_empty() || !self.bookmark_filter.is_empty() =>
                {
                    self.bookmark_category.clear();
                    self.bookmark_filter.clear();
                    self.browse_index = 0;
                    return Effect::None;
                }
                _ => {}
            }
        }
        if action == "theme_reload" {
            return Effect::ReloadTheme;
        }
        if action == "chunks" && matches!(self.view, View::Files | View::Diff) {
            if let Some(preview) = &self.preview
                && Some(preview.commit_id.as_str()) == self.selected_id()
                && let Some(file) = preview.files.get(self.file_index)
            {
                let id = preview.commit_id.clone();
                let path = file.path.clone();
                self.chunks = Some(ChunkEditor {
                    line_offset: 0,
                    id: id.clone(),
                    path: path.clone(),
                    session: None,
                    error: None,
                    index: 0,
                    selected: Default::default(),
                });
                self.view = View::Chunks;
                self.preview_focus = false;
                self.busy = true;
                return Effect::Request(crate::live::Request::Chunks { id, path });
            }
            self.notice = Some("Open a file diff before selecting hunks".into());
            return Effect::None;
        }
        if self.view == View::Chunks {
            if action == "cancel" {
                self.view = View::Files;
                self.chunks = None;
                return Effect::None;
            }
            if let Some(editor) = &mut self.chunks
                && let Some(session) = &editor.session
            {
                let hunks = session.hunks();
                let count = hunks.len();
                if action == "scroll_down" {
                    editor.line_offset = (editor.line_offset + 1)
                        .min(hunks[editor.index].patch.lines().count().saturating_sub(1));
                    return Effect::None;
                }
                if action == "scroll_up" {
                    editor.line_offset = editor.line_offset.saturating_sub(1);
                    return Effect::None;
                }
                if matches!(
                    action.as_str(),
                    "up" | "down" | "page_up" | "page_down" | "first" | "last"
                ) {
                    editor.line_offset = 0;
                }
                match action.as_str() {
                    "up" => editor.index = editor.index.saturating_sub(1),
                    "down" => editor.index = (editor.index + 1).min(count.saturating_sub(1)),
                    "page_up" => editor.index = editor.index.saturating_sub(page_size),
                    "page_down" => {
                        editor.index = (editor.index + page_size).min(count.saturating_sub(1))
                    }
                    "first" => editor.index = 0,
                    "last" => editor.index = count.saturating_sub(1),
                    "select" => {
                        if !editor.selected.remove(&editor.index) {
                            editor.selected.insert(editor.index);
                        }
                    }
                    "select_all" => {
                        if editor.selected.len() == count {
                            editor.selected.clear();
                        } else {
                            editor.selected = (0..count).collect();
                        }
                    }
                    "restore" | "split" => {
                        let selected = if editor.selected.is_empty() {
                            [editor.index].into()
                        } else {
                            editor.selected.clone()
                        };
                        let amount = selected.len();
                        self.pending_chunk = Some(crate::chunks::Request {
                            session: session.clone(),
                            selected,
                            action: if action == "restore" {
                                crate::chunks::Action::Restore
                            } else {
                                crate::chunks::Action::Split
                            },
                            description: String::new(),
                        });
                        let restore = action == "restore";
                        self.start_input(
                            if restore {
                                "chunk_restore"
                            } else {
                                "chunk_split"
                            },
                            &format!(
                                "{} {amount} selected hunk(s){}",
                                if restore { "Restore" } else { "Split" },
                                if restore { "?" } else { ": description" }
                            ),
                            String::new(),
                            restore,
                        );
                        return Effect::None;
                    }
                    _ => {}
                }
                if matches!(
                    action.as_str(),
                    "up" | "down"
                        | "page_up"
                        | "page_down"
                        | "first"
                        | "last"
                        | "select"
                        | "select_all"
                ) {
                    return Effect::None;
                }
            }
            if matches!(
                action.as_str(),
                "restore" | "split" | "select" | "select_all" | "scroll_down" | "scroll_up"
            ) {
                self.notice = Some("No editable hunks loaded".into());
                return Effect::None;
            }
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
        if self.preview_focus && self.preview_visible && matches!(action.as_str(), "first" | "last")
        {
            self.scroll = if action == "first" { 0 } else { u16::MAX };
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
            View::Operations | View::InlineOperations | View::Bookmarks | View::Git | View::Finder
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
            "search" | "search_backward" => {
                if self.view == View::Files || self.view == View::Finder {
                    self.start_input(
                        "file_filter",
                        "Filter files",
                        self.file_filter.clone(),
                        false,
                    );
                } else {
                    self.search_backwards = action == "search_backward";
                    self.start_input(
                        &action,
                        if self.search_backwards {
                            "? Search backward"
                        } else {
                            "/ Search forward"
                        },
                        self.filter.clone(),
                        false,
                    );
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
                self.view = View::Revisions;
                self.preview_focus = false;
                self.start_input(
                    &action,
                    "Set bookmark at selected revision",
                    String::new(),
                    false,
                );
                let sources = self.action_sources("set_bookmark");
                self.input.as_mut().unwrap().sources = sources;
                self.bookmark_completion = None;
                self.listing.clear();
                if let Some(id) = self.selected_id() {
                    return Effect::Request(crate::live::Request::Listing(
                        crate::browser::Query::BookmarkOperations(id.into()),
                    ));
                }
                return Effect::None;
            }
            "rebase" | "squash" => {
                self.target = Some((action.clone(), self.sources(), self.selected_files()));
                self.view = View::Revisions;
                self.notice = None;
                return Effect::None;
            }
            "undo" => {
                self.start_input("undo", "Confirm undo", String::new(), true);
                return Effect::None;
            }
            "abandon" | "restore" | "redo" => {
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
            "workspace_update" => {
                return self.command_effect(crate::commands::CommandSpec::new([
                    "workspace",
                    "update-stale",
                ]));
            }
            "workspaces" => {
                if self.pending_command || self.plugin_session.is_some() {
                    self.notice = Some("An action is still running".into());
                    return Effect::None;
                }
                self.workspace_menu = Some(workspaces::Menu::default());
                return Effect::Request(crate::live::Request::Workspaces);
            }
            "bookmarks" => {
                let Some(id) = self.selected_id().map(str::to_owned) else {
                    return Effect::None;
                };
                self.view = View::Bookmarks;
                self.preview_focus = false;
                self.bookmark_category.clear();
                self.bookmark_filter.clear();
                self.bookmark_remote = 0;
                self.bookmark_remotes = vec!["local".into()];
                self.listing.clear();
                self.browse_index = 0;
                self.busy = true;
                return Effect::Request(crate::live::Request::Listing(
                    crate::browser::Query::BookmarkOperations(id),
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
                let Some(id) = self.selected_id().map(str::to_owned) else {
                    return Effect::None;
                };
                self.view = View::Git;
                self.preview_focus = false;
                self.listing.clear();
                self.bookmark_remotes.clear();
                self.bookmark_remote = 0;
                self.bookmark_category.clear();
                self.bookmark_filter.clear();
                self.browse_index = 0;
                self.busy = true;
                return Effect::Request(crate::live::Request::Listing(
                    crate::browser::Query::GitOperations(id),
                ));
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
                self.start_input(
                    "command",
                    "Command (:q quits, :help, or jj command)",
                    String::new(),
                    false,
                );
                return Effect::None;
            }
            "shell" => {
                self.start_input("shell", "Shell command", String::new(), false);
                return Effect::None;
            }
            "history" => {
                self.output_is_diff = false;
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
        self.mouse_inner(mouse);
        self.collapse_unfocused_immutable();
    }
    fn mouse_inner(&mut self, mouse: event::MouseEvent) {
        if self.input.is_some() {
            return;
        }
        match mouse.kind {
            event::MouseEventKind::Down(event::MouseButton::Left) => {
                if let Some((_, index, offset)) = self
                    .chunk_hit_rows
                    .iter()
                    .find(|(rect, _, _)| rect.contains((mouse.column, mouse.row).into()))
                    && let Some(editor) = &mut self.chunks
                {
                    editor.index = *index;
                    editor.line_offset = *offset;
                    if mouse.modifiers.contains(KeyModifiers::CONTROL)
                        && !editor.selected.remove(index)
                    {
                        editor.selected.insert(*index);
                    }
                    return;
                }
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
                if self.view == View::Chunks
                    && let Some(editor) = &mut self.chunks
                    && let Some(session) = &editor.session
                {
                    editor.index = if down {
                        (editor.index + 1).min(session.hunks().len().saturating_sub(1))
                    } else {
                        editor.index.saturating_sub(1)
                    };
                    editor.line_offset = 0;
                } else if matches!(
                    self.view,
                    View::Operations | View::InlineOperations | View::Bookmarks | View::Git
                ) {
                    self.browse_index = if down {
                        (self.browse_index + 1).min(self.listing_rows().len().saturating_sub(1))
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
            #[cfg(test)]
            crate::browser::Query::Bookmarks => false,
            crate::browser::Query::BookmarkOperations(id) => {
                (self.view == View::Bookmarks
                    || self
                        .input
                        .as_ref()
                        .is_some_and(|input| input.action == "set_bookmark"))
                    && self.selected_id() == Some(id)
            }
            crate::browser::Query::GitOperations(id) => {
                self.view == View::Git && self.selected_id() == Some(id)
            }
            crate::browser::Query::Operations => self.view == View::Operations,
            crate::browser::Query::CommitOperations(id) => {
                self.view == View::InlineOperations && self.selected_id() == Some(id)
            }
            crate::browser::Query::Files(id) => {
                self.view == View::Finder && self.selected_id() == Some(id)
            }
        }
    }
    fn apply_bookmark(&mut self) -> Effect {
        if self.pending_command {
            return Effect::None;
        }
        if let Some(action) = self
            .listing_rows()
            .get(self.browse_index)
            .and_then(|row| row.bookmark.as_ref())
        {
            return self.command_effect(crate::commands::CommandSpec::new(action.args.clone()));
        }
        Effect::None
    }
    fn listing_rows(&self) -> Vec<&crate::browser::Row> {
        self.listing
            .iter()
            .filter(|row| self.view != View::Finder || fuzzy_match(&row.label, &self.file_filter))
            .filter(|row| {
                if !matches!(self.view, View::Bookmarks | View::Git) {
                    return true;
                }
                let Some(action) = &row.bookmark else {
                    return false;
                };
                let remote = self
                    .bookmark_remotes
                    .get(self.bookmark_remote)
                    .map(String::as_str)
                    .unwrap_or("local");
                (if remote == "local" {
                    action.local
                } else {
                    action.remote.as_deref() == Some(remote)
                        && !(action.kind == "track" && action.local)
                }) && (self.bookmark_category.is_empty() || action.kind == self.bookmark_category)
                    && row
                        .label
                        .to_lowercase()
                        .contains(&self.bookmark_filter.to_lowercase())
            })
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
        self.full_snapshot = Some(snapshot.clone());
        let mut history = if self.ancestors_expanded {
            crate::elision::with_ancestors(&snapshot.revisions, &snapshot.ancestors)
        } else {
            snapshot.revisions.clone()
        };
        for revision in &mut history {
            revision.expanded_history = self.expanded_immutable.contains(&revision.commit_id)
                || (self.ancestors_expanded && revision.outside_revset);
            if self.ancestors_expanded && revision.outside_revset {
                revision.outside_revset = false;
                revision.elide_bookmarks = false;
            }
        }
        self.expanded_immutable
            .retain(|id| history.iter().any(|r| &r.commit_id == id));
        let (mut revisions, mut hidden) =
            crate::elision::collapse(&history, &self.expanded_immutable, &self.marked);
        let mut graph = crate::graph::render(&revisions);
        if !self.ancestors_expanded
            && let Some(ancestor) = snapshot.ancestors.first()
            && let Some(boundary) = graph.last_mut().and_then(|row| row.boundary.take())
        {
            // Make the existing log boundary selectable without introducing
            // any ancestor nodes or edges into the collapsed graph.
            let mut placeholder = ancestor.clone();
            placeholder.parents.clear();
            hidden.insert(
                placeholder.commit_id.clone(),
                snapshot
                    .ancestors
                    .iter()
                    .map(|r| r.commit_id.clone())
                    .collect(),
            );
            revisions.push(placeholder);
            graph.push(crate::graph::GraphRow {
                node: boundary,
                ..Default::default()
            });
        }
        let snapshot = Snapshot {
            ancestors: Vec::new(),
            workspace_stale: snapshot.workspace_stale,
            operation: snapshot.operation,
            revisions,
        };
        self.hidden_immutable = hidden;
        let visible = self.visible();
        let old = self
            .selection
            .selected()
            .and_then(|i| visible.get(i))
            .and_then(|i| self.snapshot.as_ref().map(|s| &s.revisions[*i]))
            .map(|r| (r.commit_id.clone(), r.change_id.clone()));
        self.graph = graph;
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

pub fn run(
    mut live: Live,
    keys: crate::keybinds::Keybinds,
    theme: crate::theme::Theme,
    theme_choice: Option<String>,
    plugin_config: crate::plugins::Config,
    keybinds_path: Option<std::path::PathBuf>,
    config_source: crate::config::Source,
) -> Result<()> {
    let version_check = version::check();
    let plugin_names = plugin_config.names();
    let plugins = crate::plugins::Worker::start(plugin_config)?;
    let mut terminal = ratatui::init();
    crossterm::execute!(std::io::stdout(), event::EnableMouseCapture)?;
    let _mouse = MouseCapture;

    // ratatui also installs a panic hook to restore the terminal.
    let result = (|| -> Result<()> {
        let mut app = App {
            keys,
            theme,
            repository_path: live.root.display().to_string(),
            revset: live.initial_revset.clone(),
            ..App::default()
        };
        let mut workspace_check = std::time::Instant::now();
        loop {
            if let Ok(status) = version_check.try_recv() {
                app.jj_version = status;
            }
            if app.workspace_menu.is_some()
                && !app.pending_command
                && workspace_check.elapsed() >= Duration::from_secs(2)
                && live
                    .tasks
                    .try_send(crate::live::Request::Workspaces)
                    .is_ok()
            {
                workspace_check = std::time::Instant::now();
            }
            loop {
                match plugins.results.try_recv() {
                    Ok(result) => {
                        app.plugin_computing = false;
                        match result {
                            Ok(plan) => app.plugin_actions = plan.actions.into(),
                            Err(error) => app.plugin_error(error),
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        if app.plugin_computing {
                            app.plugin_error("Plugin worker stopped unexpectedly".into());
                        }
                        break;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                }
            }
            if let Some(call) = app.plugin_call.take() {
                match plugins.calls.try_send(call) {
                    Ok(()) => app.plugin_computing = true,
                    Err(error) => app.plugin_error(error.to_string()),
                }
            }
            for update in live.updates.try_iter() {
                match update {
                    Update::Workspaces(result) => match result {
                        Ok(entries) => {
                            if let Some(menu) = &mut app.workspace_menu {
                                let selected =
                                    menu.entries.get(menu.index).map(|entry| entry.name.clone());
                                menu.index = selected
                                    .and_then(|name| {
                                        entries.iter().position(|entry| entry.name == name)
                                    })
                                    .or_else(|| entries.iter().position(|entry| entry.current))
                                    .unwrap_or(0);
                                menu.entries = entries;
                            }
                        }
                        Err(error) => app.error = Some(error),
                    },
                    Update::PluginFinished(result) => {
                        app.busy = false;
                        app.pending_command = false;
                        app.requested = None;
                        app.resume_plugin(crate::plugins::Response::result(result));
                    }
                    Update::Chunks { id, path, result } => {
                        app.busy = false;
                        if app.view == View::Chunks
                            && let Some(editor) = &mut app.chunks
                            && editor.id == id
                            && editor.path == path
                        {
                            match result {
                                Ok(session) => editor.session = Some(session),
                                Err(error) => editor.error = Some(error),
                            }
                        }
                    }
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
                                View::Git => app
                                    .selected_id()
                                    .map(|id| crate::browser::Query::GitOperations(id.into())),
                                View::Bookmarks => app
                                    .selected_id()
                                    .map(|id| crate::browser::Query::BookmarkOperations(id.into())),
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
                                if matches!(
                                    list.query,
                                    crate::browser::Query::BookmarkOperations(_)
                                        | crate::browser::Query::GitOperations(_)
                                ) {
                                    app.bookmark_remotes = list.remotes;
                                    app.bookmark_remote = app
                                        .bookmark_remote
                                        .min(app.bookmark_remotes.len().saturating_sub(1));
                                }
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
                        if command.starts_with("jj workspace ") && app.workspace_menu.is_some() {
                            let _ = live.tasks.try_send(crate::live::Request::Workspaces);
                        }
                        app.busy = false;
                        app.pending_command = false;
                        match result {
                            Ok(output) => {
                                if command.ends_with("selected hunks") {
                                    app.view = View::Files;
                                    app.chunks = None;
                                }
                                if (app.view == View::Bookmarks
                                    && command.starts_with("jj bookmark "))
                                    || (app.view == View::Git && command.starts_with("jj git "))
                                {
                                    app.view = View::Revisions;
                                }
                                app.output_is_diff = false;
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
            let plugin_effect = app.next_plugin_effect();
            app.collapse_unfocused_immutable();
            terminal.draw(|frame| draw(frame, &mut app))?;
            let effect = if let Some(effect) = plugin_effect {
                effect
            } else {
                if !event::poll(Duration::from_millis(100))? {
                    continue;
                }
                let key = match event::read()? {
                    Event::Key(key) => key,
                    Event::Mouse(mouse) => {
                        if app.workspace_menu.is_none()
                            && app.plugin_choice.is_none()
                            && !app
                                .input
                                .as_ref()
                                .is_some_and(|input| input.action == "plugin_prompt")
                        {
                            app.mouse(mouse);
                        }
                        continue;
                    }
                    _ => continue,
                };
                let page_size = terminal.size()?.height.saturating_sub(6) as usize / 2;
                app.handle_key(key, page_size)
            };
            match effect {
                Effect::SwitchWorkspace(path) => {
                    let next = (|| -> Result<_> {
                        let target = crate::repository::Repository::open(&path, live.limit)?;
                        anyhow::ensure!(
                            target.repo_path().canonicalize()? == live.repo_path.canonicalize()?,
                            "Workspace no longer belongs to this repository"
                        );
                        let keys = crate::keybinds::Keybinds::load_with_plugins(
                            &path,
                            keybinds_path.as_deref(),
                            &plugin_names,
                        )?;
                        let theme = crate::theme::Theme::load(&path, theme_choice.as_deref())?;
                        let auto_snapshot = config_source.load(&path)?.auto_snapshot;
                        let next = crate::live::start(
                            path,
                            live.limit,
                            auto_snapshot,
                            Some(app.revset.clone()),
                            true,
                        )?;
                        Ok((next, keys, theme))
                    })();
                    match next {
                        Ok((next, keys, theme)) => {
                            let history = std::mem::take(&mut app.history);
                            app = App {
                                keys,
                                theme,
                                history,
                                repository_path: next.root.display().to_string(),
                                revset: next.initial_revset.clone(),
                                notice: Some(format!("Switched to {}", next.root.display())),
                                follow_working_copy: true,
                                ..App::default()
                            };
                            live = next;
                        }
                        Err(error) => {
                            app.error = Some(format!("Cannot switch workspace: {error:#}"))
                        }
                    }
                }
                Effect::ReloadTheme => {
                    match crate::theme::Theme::load(&live.root, theme_choice.as_deref()) {
                        Ok(theme) => {
                            app.theme = theme;
                            app.notice = Some("Theme reloaded".into());
                            app.error = None;
                        }
                        Err(error) => app.error = Some(format!("{error:#}")),
                    }
                }
                Effect::Quit => break,
                Effect::Refresh => {
                    let _ = live.refresh.try_send(());
                }
                Effect::Request(request) => {
                    if let Err(error) = live.tasks.try_send(request) {
                        app.busy = false;
                        app.pending_command = false;
                        app.error = Some(error.to_string());
                        if app.plugin_session.is_some() {
                            app.plugin_error(error.to_string());
                        }
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

fn prompt_lines(app: &App, prefix: &str, width: usize) -> Vec<Line<'static>> {
    let Some(input) = &app.input else {
        return Vec::new();
    };
    if matches!(
        input.action.as_str(),
        "set_bookmark" | "command" | "abandon" | "undo"
    ) {
        return Vec::new();
    }
    let mut text = Vec::new();
    if input.confirm {
        text.push(input.label.clone());
        if !input.text.is_empty() {
            text.push(input.text.clone());
        }
        if !input.files.is_empty() {
            text.push(format!("Files: {}", input.files.join(", ")));
        }
    } else {
        text.push(input.visible());
    }
    let prefix = format!(
        "{prefix}{}",
        if input.action == "describe" { "" } else { "  " }
    );
    let available = width
        .saturating_sub(Line::raw(prefix.clone()).width())
        .max(1);
    let mut result = Vec::new();
    for text in text {
        let mut line = String::new();
        let mut used = 0;
        for c in text.chars().chain(std::iter::once('\n')) {
            let size = Span::raw(c.to_string()).width();
            if c == '\n' || used + size > available {
                result.push(Line::from(vec![
                    Span::styled(
                        prefix.clone(),
                        app.snapshot
                            .as_ref()
                            .and_then(|s| app.selection.selected().and_then(|i| s.revisions.get(i)))
                            .map(revision_graph_style)
                            .unwrap_or_else(|| Style::default().fg(Color::Reset)),
                    ),
                    Span::styled(
                        std::mem::take(&mut line),
                        Style::default().fg(Color::Yellow),
                    ),
                ]));
                used = 0;
            }
            if c != '\n' {
                line.push(c);
                used += size;
            }
        }
    }
    result
}

fn draw(frame: &mut ratatui::Frame, app: &mut App) {
    app.hit_rows.clear();
    app.operation_hit_rows.clear();
    app.chunk_hit_rows.clear();
    let workspace_input = if app.workspace_menu.is_some() {
        app.input.take()
    } else {
        None
    };
    draw_body(frame, app);
    if app.workspace_menu.is_some() {
        app.input = workspace_input;
    }
    let inline = match app.view {
        View::Revisions | View::Files | View::InlineOperations => app.selected_id().is_some(),
        View::Chunks => {
            app.selected_id().is_some()
                && app
                    .chunks
                    .as_ref()
                    .is_some_and(|editor| editor.session.is_some())
        }
        View::Operations | View::Finder => !app.listing_rows().is_empty(),
        View::Bookmarks | View::Git => true,
        _ => false,
    };
    if let Some(input) = &app.input
        && !inline
        && app.workspace_menu.is_none()
        && !matches!(input.action.as_str(), "command" | "abandon" | "undo")
    {
        let area = frame.area();
        let height = area.height.saturating_sub(2).min(9);
        let area = ratatui::layout::Rect::new(
            area.x,
            area.y + area.height.saturating_sub(height + 2),
            area.width,
            height,
        );
        frame.render_widget(ratatui::widgets::Clear, area);
        let text = if input.confirm {
            let mut lines = vec![input.label.clone()];
            if !input.text.is_empty() {
                lines.push(input.text.clone());
            }
            if !input.sources.is_empty() {
                lines.push(format!("Sources: {}", input.sources.join(", ")));
            }
            if !input.files.is_empty() {
                lines.push(format!("Files: {}", input.files.join(", ")));
            }
            lines.join("\n")
        } else {
            input.visible()
        };
        frame.render_widget(
            Paragraph::new(text)
                .wrap(ratatui::widgets::Wrap { trim: false })
                .block(Block::bordered()),
            area,
        );
    }
    if let Some(choice) = &app.plugin_choice {
        let area = frame.area();
        let width = area.width.saturating_sub(4).min(80);
        let height = area
            .height
            .saturating_sub(4)
            .min(
                choice
                    .options
                    .len()
                    .saturating_add(2)
                    .min(u16::MAX as usize) as u16,
            )
            .max(area.height.min(3));
        let popup = ratatui::layout::Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(ratatui::widgets::Clear, popup);
        let mut state = ListState::default().with_selected(Some(choice.index));
        frame.render_stateful_widget(
            List::new(choice.options.clone())
                .block(Block::bordered().title(choice.title.clone()))
                .highlight_style(Style::default().bg(Color::DarkGray))
                .highlight_symbol(""),
            popup,
            &mut state,
        );
    }
    if app.workspace_menu.is_some() {
        workspaces::draw(frame, app);
        let area = frame.area();
        frame.render_widget(
            Paragraph::new(footer_hints(app, area.width as usize)),
            ratatui::layout::Rect::new(
                area.x,
                area.y + area.height.saturating_sub(2),
                area.width,
                area.height.min(1),
            ),
        );
    }
    if app.workspace_menu.is_some()
        && let Some(input) = &app.input
        && !input.confirm
    {
        let area = frame.area();
        frame.render_widget(
            Paragraph::new(input.label.clone()).style(Style::default().fg(Color::Yellow)),
            ratatui::layout::Rect::new(
                area.x,
                area.bottom().saturating_sub(1),
                area.width,
                area.height.min(1),
            ),
        );
    }
    if app
        .input
        .as_ref()
        .is_some_and(|input| input.action == "undo")
    {
        let area = frame.area();
        let width = area.width.saturating_sub(4).min(68);
        let height = area.height.saturating_sub(4).min(4);
        let popup = ratatui::layout::Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(ratatui::widgets::Clear, popup);
        frame.render_widget(
            Paragraph::new("Undo the last operation in this repository?")
                .wrap(ratatui::widgets::Wrap { trim: false })
                .block(Block::bordered().title(" Confirm undo ")),
            popup,
        );
    }
    app.theme.apply(frame.buffer_mut());
}

fn footer_mode(app: &App) -> &'static str {
    if app.workspace_menu.is_some() {
        return "Workspaces";
    }
    if app.input.is_some() {
        return "Input";
    }
    if app.target.is_some() {
        return "Choose destination";
    }
    if app.preview_focus && app.preview_visible {
        return "Preview";
    }
    match app.view {
        View::Revisions => "Revisions",
        View::Files => "Files",
        View::Chunks => "Hunks",
        View::Operations => "Operations",
        View::InlineOperations => "Related operations",
        View::Diff => "Diff",
        View::OperationDiff => "Snapshot diff",
        View::Bookmarks => "Bookmarks",
        View::Finder => "File finder",
        View::Git => "Git",
        View::Help => "Help",
        View::Text => "Output",
    }
}

fn footer_hints(app: &App, width: usize) -> Line<'static> {
    if app.workspace_menu.is_some() {
        if app.input.is_none()
            && !app.key_sequence.is_empty()
            && app.key_sequence_scope == "workspaces"
        {
            return render_footer_hints(
                app,
                width,
                app.keys
                    .continuations("workspaces", &app.key_sequence)
                    .into_iter()
                    .map(|(key, action)| {
                        (
                            key,
                            action.trim_start_matches("workspace_").replace('_', " "),
                        )
                    }),
            );
        }
        let (scope, hints): (&str, &[(&str, &str)]) = if app.input.is_some() {
            ("input", &[("submit", "apply"), ("cancel", "cancel")])
        } else {
            (
                "workspaces",
                &[
                    ("workspace_switch", "switch"),
                    ("workspace_add", "add"),
                    ("workspace_rename", "rename current"),
                    ("workspace_forget", "forget"),
                    ("workspace_remove", "remove directory"),
                    ("workspace_update", "update stale"),
                    ("cancel", "back"),
                ],
            )
        };
        return render_footer_hints(
            app,
            width,
            hints.iter().filter_map(|(action, label)| {
                app.keys
                    .hint(scope, action)
                    .map(|key| (key, (*label).into()))
            }),
        );
    }

    if app.plugin_choice.is_some() {
        return render_footer_hints(
            app,
            width,
            [
                ("down", "next"),
                ("up", "previous"),
                ("submit", "choose"),
                ("cancel", "cancel"),
            ]
            .into_iter()
            .filter_map(|(action, label)| {
                app.keys
                    .hint("choice", action)
                    .map(|key| (key, label.into()))
            }),
        );
    }

    let scope = if app.selected_elision().is_some()
        && matches!(
            app.view,
            View::Revisions | View::InlineOperations | View::Files
        ) {
        "elision"
    } else {
        match app.view {
            View::Revisions => "revisions",
            View::Files => "files",
            View::Chunks => "chunks",
            View::Operations | View::InlineOperations => "operations",
            View::Bookmarks => "bookmarks",
            View::Finder => "finder",
            View::Git => "git",
            _ => "view",
        }
    };
    if app.input.is_none() && !app.key_sequence.is_empty() && app.key_sequence_scope == scope {
        let mut hints: Vec<_> = app
            .keys
            .continuations(scope, &app.key_sequence)
            .into_iter()
            .map(|(key, action)| {
                let label = match action.as_str() {
                    "first" => match app.view {
                        View::Revisions => "first revision",
                        View::Files | View::Finder => "first file",
                        View::Chunks => "first hunk",
                        View::Operations | View::InlineOperations => "first operation",
                        _ => "first",
                    }
                    .into(),
                    "git" => "Git".into(),
                    "theme_reload" => "reload theme".into(),
                    "focus" => "switch focus".into(),
                    _ => action.trim_start_matches("plugin.").replace('_', " "),
                };
                (key, label)
            })
            .collect();
        if let Some(key) = app.keys.hint(scope, "cancel") {
            hints.push((key, "cancel".into()));
        }
        return render_footer_hints(app, width, hints);
    }
    let mut hints: Vec<(&str, &str, &str)> = Vec::new();
    if app.snapshot.as_ref().is_some_and(|s| s.workspace_stale) && app.input.is_none() {
        hints.push(("global", "workspace_update", "update stale workspace"));
    }
    if app.view == View::Revisions
        && app.input.is_none()
        && app.target.is_none()
        && (!app.expanded_immutable.is_empty() || app.ancestors_expanded)
    {
        hints.push((
            "revisions",
            "cancel",
            if app.expanded_immutable.is_empty() {
                "hide ancestors"
            } else {
                "hide immutable commits"
            },
        ));
    }
    if let Some(input) = &app.input {
        hints.extend([
            (
                "input",
                "submit",
                if input.confirm { "confirm" } else { "apply" },
            ),
            ("input", "cancel", "cancel"),
        ]);
        if input.action == "set_bookmark" {
            hints.extend([
                ("input", "complete", "complete"),
                ("input", "complete_back", "previous"),
            ]);
        }
        if !input.confirm {
            hints.extend([
                ("input", "delete_word", "delete word"),
                ("input", "delete_to_start", "clear to start"),
                ("input", "newline", "newline"),
            ]);
        }
    } else if scope == "elision" {
        let ancestor_boundary = app
            .snapshot
            .as_ref()
            .and_then(|s| app.selection.selected().and_then(|i| s.revisions.get(i)))
            .is_some_and(|r| r.outside_revset);
        hints.extend([
            (
                "elision",
                "expand",
                if ancestor_boundary {
                    "show remote ancestry"
                } else {
                    "show hidden commits"
                },
            ),
            ("elision", "down", "down"),
            ("elision", "up", "up"),
            ("elision", "working_copy", "working copy"),
            ("global", "help", "help"),
        ]);
    } else if app.target.is_some() {
        hints.extend([
            ("revisions", "down", "next destination"),
            ("revisions", "up", "previous"),
            ("input", "submit", "apply"),
            ("global", "cancel", "cancel"),
        ]);
    } else if app.preview_focus && app.preview_visible {
        hints.extend([
            (scope, "down", "scroll down"),
            (scope, "up", "scroll up"),
            (scope, "half_page_down", "half down"),
            (scope, "half_page_up", "half up"),
            (scope, "focus", "back to graph"),
        ]);
    } else {
        match app.view {
            View::Revisions => hints.extend([
                (scope, "down", "down"),
                (scope, "up", "up"),
                (scope, "details", "files"),
                (scope, "diff", "diff"),
                (scope, "set_bookmark", "bookmark"),
                (scope, "bookmarks", "bookmark actions"),
                (scope, "describe", "describe"),
                (scope, "operations", "operations"),
                (scope, "new", "new"),
                (scope, "select", "mark"),
                ("global", "help", "help"),
            ]),
            View::Files => hints.extend([
                (scope, "down", "down"),
                (scope, "up", "up"),
                (scope, "diff", "diff"),
                (scope, "chunks", "hunks"),
                (scope, "select", "mark"),
                (scope, "restore", "restore"),
                (scope, "split", "split"),
                (scope, "cancel", "back"),
            ]),
            View::Chunks
                if app
                    .chunks
                    .as_ref()
                    .and_then(|editor| editor.session.as_ref())
                    .is_none() =>
            {
                hints.push((scope, "cancel", "back"));
            }
            View::Chunks => hints.extend([
                (scope, "down", "next hunk"),
                (scope, "up", "previous"),
                (scope, "select", "mark"),
                (scope, "select_all", "all"),
                (scope, "restore", "restore"),
                (scope, "split", "split"),
                (scope, "scroll_down", "scroll"),
                (scope, "cancel", "back"),
            ]),
            View::Operations | View::InlineOperations => {
                hints.extend([(scope, "down", "down"), (scope, "up", "up")]);
                if app
                    .listing
                    .get(app.browse_index)
                    .and_then(|row| row.operation.as_ref())
                    .is_some_and(|op| op.snapshot)
                {
                    hints.push((scope, "diff", "snapshot diff"));
                }
                if !app.listing.is_empty() {
                    hints.push((scope, "operation_restore", "restore operation"));
                }
                hints.push((
                    scope,
                    if app.view == View::InlineOperations {
                        "full_operations"
                    } else {
                        "operations"
                    },
                    if app.view == View::InlineOperations {
                        "all operations"
                    } else {
                        "related operations"
                    },
                ));
                hints.push((scope, "cancel", "back"));
            }
            View::Bookmarks => hints.extend([
                (scope, "bookmark_apply", "apply"),
                (scope, "bookmark_move", "move"),
                (scope, "bookmark_delete", "delete"),
                (scope, "bookmark_forget", "forget"),
                (scope, "bookmark_track", "track"),
                (scope, "bookmark_untrack", "untrack"),
                (scope, "search", "filter"),
                (scope, "bookmark_next_remote", "remote"),
                (scope, "cancel", "back"),
            ]),
            View::Finder => hints.extend([
                (scope, "down", "down"),
                (scope, "up", "up"),
                (scope, "search", "filter"),
                (scope, "open_file", "open"),
                (scope, "cancel", "back"),
            ]),
            View::Git => hints.extend([
                (scope, "bookmark_apply", "apply"),
                (scope, "fetch", "fetch"),
                (scope, "push", "push"),
                (scope, "remote_add", "add remote"),
                (scope, "remote_remove", "remove remote"),
                (scope, "bookmark_next_remote", "remote"),
                (scope, "search", "filter"),
                (scope, "cancel", "back"),
            ]),
            View::Diff | View::OperationDiff | View::Help | View::Text => {
                hints.extend([
                    (scope, "down", "scroll down"),
                    (scope, "up", "scroll up"),
                    (scope, "half_page_down", "half down"),
                    (scope, "half_page_up", "half up"),
                ]);
                if app.view == View::Diff {
                    hints.push((scope, "chunks", "hunks"));
                }
                hints.push((scope, "cancel", "back"));
            }
        }
    }
    let hints = hints.into_iter().filter_map(|(scope, action, label)| {
        app.keys
            .hint(scope, action)
            .map(|key| (key, label.to_owned()))
    });
    let plugin_hints = if app.input.is_none() && app.target.is_none() {
        app.keys.plugin_hints(scope)
    } else {
        Vec::new()
    };
    render_footer_hints(app, width, hints.chain(plugin_hints))
}

fn render_footer_hints(
    app: &App,
    width: usize,
    hints: impl IntoIterator<Item = (String, String)>,
) -> Line<'static> {
    let (key_color, action_color, separator_color) = app.theme.hint_colors();
    let mut spans = Vec::new();
    let mut used = 0;
    for (key, label) in hints {
        let key = if key.split_whitespace().all(|part| part.chars().count() == 1) {
            key.replace(' ', "")
        } else {
            key
        };
        let separator = if spans.is_empty() { "" } else { "  ·  " };
        let needed = Line::raw(format!("{separator}{key} {label}")).width();
        if used + needed > width {
            continue;
        }
        spans.push(Span::styled(
            separator,
            Style::default().fg(separator_color),
        ));
        spans.push(Span::styled(
            key,
            Style::default().fg(key_color).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {label}"),
            Style::default().fg(action_color),
        ));
        used += needed;
    }
    Line::from(spans)
}

fn footer_action_status(app: &App) -> Option<String> {
    if let Some(input) = &app.input
        && input.action == "abandon"
    {
        return Some(format!(
            "{} · {}",
            input.label,
            input
                .sources
                .iter()
                .map(|id| &id[..id.len().min(8)])
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(input) = &app.input
        && !input.confirm
        && !matches!(input.action.as_str(), "command" | "set_bookmark")
    {
        return Some(input.label.clone());
    }
    if let Some((action, sources, _)) = &app.target {
        let label = if action == "rebase" {
            "Rebase"
        } else {
            "Squash"
        };
        let destination = if app.selected_elision().is_some() {
            "hidden commits (expand to choose)".into()
        } else {
            app.snapshot
                .as_ref()
                .and_then(|snapshot| {
                    app.selection
                        .selected()
                        .and_then(|i| snapshot.revisions.get(i))
                })
                .map(|revision| revision.change_id[..revision.change_id.len().min(8)].to_owned())
                .unwrap_or_else(|| "no destination".into())
        };
        return Some(format!(
            "{label} {} revision(s) → {destination}",
            sources.len()
        ));
    }
    None
}

fn draw_body(frame: &mut ratatui::Frame, app: &mut App) {
    let stale = app.snapshot.as_ref().is_some_and(|s| s.workspace_stale);
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(if stale { 2 } else { 1 }),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .areas(frame.area());
    let operation = app
        .snapshot
        .as_ref()
        .map(|s| short(&s.operation))
        .unwrap_or("loading");
    let version = app.jj_version.line();
    let [header_main, header_version] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length((version.width() as u16).min(header.width.saturating_sub(10))),
    ])
    .areas(header);
    frame.render_widget(
        Paragraph::new(version).alignment(ratatui::layout::Alignment::Right),
        header_version,
    );
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
        header_main,
    );
    if stale {
        frame.render_widget(
            Paragraph::new(format!(
                " Working copy STALE · {} update workspace · graph shows repository state",
                app.keys.label("global", "workspace_update")
            ))
            .style(Style::default().fg(Color::Yellow)),
            ratatui::layout::Rect::new(
                header.x,
                header.y + 1,
                header.width,
                header.height.saturating_sub(1),
            ),
        );
    }
    let action_status = footer_action_status(app);
    let mut status = app
        .error
        .as_deref()
        .or(action_status.as_deref())
        .or(app.notice.as_deref())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            format!(
                "{} · {} · revset: {}",
                footer_mode(app),
                if app.busy { "Working…" } else { "LIVE" },
                app.revset
            )
        });
    let command_input = app.input.as_ref().filter(|input| input.action == "command");
    if let Some(input) = command_input {
        status = format!(":{}", input.visible());
        // Keep the cursor visible when a command exceeds the terminal width.
        let before = format!(":{}", &input.text[..input.cursor]);
        let skip = Line::raw(before)
            .width()
            .saturating_sub(footer.width.saturating_sub(1) as usize);
        let mut columns = 0;
        status = status
            .chars()
            .skip_while(|c| {
                if columns >= skip {
                    false
                } else {
                    columns += Span::raw(c.to_string()).width();
                    true
                }
            })
            .collect();
    }
    if command_input.is_some()
        && footer.height >= 2
        && footer.width > 0
        && let Some(index) = status.find('▏')
    {
        frame.set_cursor_position((
            footer.x
                + Line::raw(&status[..index])
                    .width()
                    .min(footer.width as usize - 1) as u16,
            footer.bottom() - 1,
        ));
    }
    frame.render_widget(
        Paragraph::new(vec![
            footer_hints(app, footer.width as usize),
            Line::styled(
                status,
                Style::default().fg(if command_input.is_some() {
                    Color::Yellow
                } else if app.error.is_some() {
                    Color::Red
                } else if action_status.is_some() {
                    Color::Yellow
                } else {
                    Color::DarkGray
                }),
            ),
        ]),
        footer,
    );
    if app.view == View::Operations {
        draw_operations(frame, body, app);
        return;
    }
    if matches!(app.view, View::Bookmarks | View::Git) {
        draw_graph(frame, body, app);
        app.hit_rows.clear();
        draw_bookmarks(frame, body, app);
        return;
    }
    if app.view == View::Finder {
        let rows = app.listing_rows();
        let items: Vec<_> = rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let mut lines = vec![Line::raw(row.label.clone())];
                if index == app.browse_index {
                    lines.extend(prompt_lines(
                        app,
                        "  ",
                        body.width.saturating_sub(4) as usize,
                    ));
                }
                ListItem::new(lines)
            })
            .collect();
        let mut state = ListState::default();
        if !items.is_empty() {
            state.select(Some(app.browse_index.min(items.len() - 1)));
        }
        frame.render_stateful_widget(
            List::new(items)
                .highlight_style(Style::default().bg(Color::DarkGray))
                .highlight_symbol(""),
            body,
            &mut state,
        );
        return;
    }
    if matches!(
        app.view,
        View::Diff | View::OperationDiff | View::Help | View::Text | View::Git
    ) {
        let text = match app.view {
            View::Diff => app.preview_text(),
            View::OperationDiff => match app
                .operation_diff
                .as_ref()
                .and_then(|diff| diff.result.as_ref())
            {
                Some(Ok(text)) => text.clone(),
                Some(Err(error)) => format!("Cannot load operation diff: {error}"),
                None => "Loading operation diff…".into(),
            },
            View::Help => format!(
                "{}\n\nRecent working-tree events\n{}",
                app.keys.help(),
                app.files.iter().cloned().collect::<Vec<_>>().join("\n")
            ),
            _ => app.output.clone(),
        };
        draw_text(
            frame,
            body,
            &text,
            &mut app.scroll,
            matches!(app.view, View::Diff | View::OperationDiff)
                || (app.view == View::Text && app.output_is_diff),
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
        draw_text(frame, area, &text, &mut app.scroll, true);
    }
}

// Keep ID disambiguation independent of the visible graph window. jj-lib supplies
// repository-wide prefix lengths; eight characters is jj's default display width.
fn log_id(id: &str, prefix_len: usize, color: Color) -> Vec<Span<'static>> {
    log_id_width(id, prefix_len, color, 8)
}
fn log_id_width(id: &str, prefix_len: usize, color: Color, width: usize) -> Vec<Span<'static>> {
    let end = id.len().min(prefix_len.max(width));
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

fn revision_graph_style(revision: &crate::repository::Revision) -> Style {
    Style::default().fg(if revision.expanded_history {
        Color::DarkGray
    } else {
        Color::Reset
    })
}

fn graph_node_spans(node: &str, style: Style) -> Vec<Span<'static>> {
    if let Some((before, after)) = node.split_once('@') {
        vec![
            Span::styled(before.to_owned(), style),
            Span::styled("@", style.fg(Color::Green)),
            Span::styled(after.to_owned(), style),
        ]
    } else {
        vec![Span::styled(node.to_owned(), style)]
    }
}

fn revision_header(
    revision: &crate::repository::Revision,
    node: &str,
    marked: bool,
    editor: Option<&crate::input::Input>,
) -> Line<'static> {
    let wc = revision.working_copy;
    let mut spans = graph_node_spans(node, revision_graph_style(revision));
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
    if let Some(input) = editor {
        spans.push(Span::styled(
            input.visible(),
            Style::default()
                .fg(Color::LightMagenta)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
    }
    if !revision.tags.is_empty() {
        spans.push(Span::styled(
            format!("{} ", revision.tags.join(" ")),
            Style::default().fg(if wc {
                Color::LightMagenta
            } else {
                Color::Magenta
            }),
        ));
    }
    for workspace in &revision.workspaces {
        spans.push(Span::styled(
            format!("{workspace} "),
            Style::default().fg(Color::Green),
        ));
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
    if revision.divergent {
        spans.push(Span::styled(
            " (divergent)",
            Style::default().fg(if wc { Color::LightRed } else { Color::Red }),
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
        revision_graph_style(revision),
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
    let mut spans = vec![Span::styled(
        outer.to_owned(),
        Style::default().fg(Color::Reset),
    )];
    spans.extend(graph_node_spans(&graph.node, style.fg(Color::Reset)));
    spans.push(Span::raw(" "));
    spans.extend(log_id_width(&row.id, op.prefix_len, blue, 12));
    if !op.bookmarks.is_empty() {
        spans.push(Span::styled(
            format!(" {}", op.bookmarks.join(" ")),
            Style::default().fg(Color::Magenta),
        ));
    }
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
            Span::styled(outer.to_owned(), Style::default().fg(Color::Reset)),
            Span::raw(format!("{} ", graph.edge)),
            Span::raw(op.description.lines().next().unwrap_or_default().to_owned()),
        ])
        .style(style),
    );
    for attribute in &op.attributes {
        for text in attribute.lines() {
            lines.push(
                Line::from(vec![
                    Span::styled(outer.to_owned(), Style::default().fg(Color::Reset)),
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
            Span::styled(outer.to_owned(), Style::default().fg(Color::Reset)),
            Span::styled(boundary.clone(), Style::default().fg(Color::Reset)),
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
    let block = Block::default();
    let content = block.inner(area);
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
        if index == app.browse_index && app.input.is_some() {
            for line in prompt_lines(app, &graph.continuation, content.width as usize) {
                selected = Some(items.len());
                indices.push(index);
                items.push(ListItem::new(line));
            }
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
            .block(block)
            .highlight_style(Style::default().bg(Color::Indexed(236)))
            .highlight_symbol(""),
        area,
        &mut app.operation_selection,
    );
    for (line, index) in indices
        .into_iter()
        .skip(app.operation_selection.offset())
        .take(content.height as usize)
        .enumerate()
    {
        app.operation_hit_rows.push((
            ratatui::layout::Rect::new(content.x, content.y + line as u16, content.width, 1),
            index,
        ));
    }
}

fn draw_graph(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &mut App) {
    let block = Block::default();
    let content = block.inner(area);
    let mut items = Vec::new();
    let mut selected_row = None;
    let mut targets = Vec::new();
    let mut operation_rows = std::collections::HashMap::new();
    let mut chunk_rows = std::collections::HashMap::new();
    let mut bookmark_cursor = None;
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
            if let Some(hidden) = app.hidden_immutable.get(&revision.commit_id) {
                targets.push((i, None));
                items.push(ListItem::new(Line::from(vec![
                    Span::styled(
                        graph.node.replace(['○', '@'], "~"),
                        revision_graph_style(revision),
                    ),
                    Span::styled(
                        if revision.outside_revset {
                            String::new()
                        } else {
                            format!(
                                "{} immutable revision{}",
                                hidden.len(),
                                if hidden.len() == 1 { "" } else { "s" }
                            )
                        },
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        if revision.outside_revset || revision.bookmarks.is_empty() {
                            String::new()
                        } else {
                            format!("  {}", revision.bookmarks.join(" "))
                        },
                        Style::default().fg(Color::Magenta),
                    ),
                ])));
                if graph.edge.chars().any(|c| !matches!(c, ' ' | '│')) {
                    targets.push((i, None));
                    items.push(ListItem::new(Line::styled(
                        graph.edge.clone(),
                        revision_graph_style(revision),
                    )));
                }
                if let Some(boundary) = &graph.boundary {
                    targets.push((i, None));
                    items.push(ListItem::new(format!(
                        "{boundary} ancestors outside loaded history"
                    )));
                }
                continue;
            }
            targets.push((i, None));
            let mut header = revision_header(
                revision,
                &graph.node,
                app.marked.contains(&revision.commit_id),
                app.input
                    .as_ref()
                    .filter(|input| selected && input.action == "set_bookmark"),
            );
            if selected
                && app
                    .input
                    .as_ref()
                    .is_some_and(|input| input.action == "set_bookmark")
            {
                let text = header.to_string();
                if let Some(index) = text.find('▏') {
                    bookmark_cursor = Some((items.len(), Line::raw(&text[..index]).width()));
                }
            }
            if let Some((action, sources, _)) = &app.target {
                if sources.contains(&revision.change_id) {
                    header.spans.push(Span::styled(
                        format!(" [{action} source]"),
                        Style::default().fg(Color::Yellow),
                    ));
                }
                if selected {
                    header.spans.push(Span::styled(
                        format!(" [{action} destination]"),
                        Style::default().fg(Color::Cyan),
                    ));
                }
            } else if let Some(input) = &app.input
                && input.confirm
                && input.action != "undo"
                && input.sources.contains(&revision.change_id)
            {
                header.spans.push(Span::styled(
                    format!(" [{}]", input.action),
                    Style::default().fg(Color::Yellow),
                ));
            }
            items.push(ListItem::new(header).style(style));
            let describing = selected
                && app
                    .input
                    .as_ref()
                    .is_some_and(|input| input.action == "describe");
            let mut prompt_focus = None;
            if describing {
                for line in prompt_lines(app, &graph.edge, content.width as usize) {
                    prompt_focus = Some(items.len());
                    targets.push((i, None));
                    items.push(ListItem::new(line));
                }
            } else if !revision.root {
                targets.push((i, None));
                items.push(ListItem::new(revision_description(revision, &graph.edge)).style(style));
            }
            if selected
                && !describing
                && app.input.is_some()
                && app.view != View::Chunks
                && app.view != View::Bookmarks
                && (app.view != View::InlineOperations || app.listing.is_empty())
            {
                for line in prompt_lines(app, &graph.continuation, content.width as usize) {
                    prompt_focus = Some(items.len());
                    targets.push((i, None));
                    items.push(ListItem::new(line));
                }
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
                    for mut line in
                        operation_lines(row, op_graph, &format!("{}  ", graph.continuation))
                    {
                        if let Some(outer) = line.spans.first_mut() {
                            outer.style = revision_graph_style(revision);
                        }
                        operation_rows.insert(items.len(), index);
                        targets.push((i, None));
                        items.push(ListItem::new(line));
                    }
                    if index == app.browse_index && app.input.is_some() {
                        for line in prompt_lines(
                            app,
                            &format!("{}  {}", graph.continuation, op_graph.continuation),
                            content.width as usize,
                        ) {
                            prompt_focus = Some(items.len());
                            targets.push((i, None));
                            items.push(ListItem::new(line));
                        }
                    }
                }
            }
            if selected && app.view == View::Files {
                let inline = |text: String| {
                    ListItem::new(Line::from(vec![
                        Span::styled(graph.continuation.clone(), revision_graph_style(revision)),
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
            if selected
                && app.view == View::Chunks
                && let Some(editor) = &app.chunks
            {
                targets.push((i, None));
                items.push(ListItem::new(format!(
                    "{}  Hunks · {} · {} mark · {} restore · {} split",
                    graph.continuation,
                    editor.path,
                    app.keys.label("chunks", "select"),
                    app.keys.label("chunks", "restore"),
                    app.keys.label("chunks", "split")
                )));
                if let Some(session) = &editor.session {
                    for (index, hunk) in session.hunks().iter().enumerate() {
                        if index == editor.index {
                            selected_row = Some(
                                items.len()
                                    + editor
                                        .line_offset
                                        .min(hunk.patch.lines().count().saturating_sub(1)),
                            );
                        }
                        for (line_number, text) in hunk.patch.lines().enumerate() {
                            chunk_rows.insert(items.len(), (index, line_number));
                            let color = if text.starts_with('+') {
                                Color::Green
                            } else if text.starts_with('-') {
                                Color::Red
                            } else {
                                Color::Cyan
                            };
                            targets.push((i, None));
                            items.push(ListItem::new(Line::from(vec![
                                Span::styled(
                                    format!(
                                        "{}  {} ",
                                        graph.continuation,
                                        if line_number == 0 && editor.selected.contains(&index) {
                                            "[x]"
                                        } else if line_number == 0 {
                                            "[ ]"
                                        } else {
                                            "   "
                                        }
                                    ),
                                    revision_graph_style(revision),
                                ),
                                Span::styled(text.to_owned(), Style::default().fg(color)),
                            ])));
                        }
                        if index == editor.index && app.input.is_some() {
                            for line in
                                prompt_lines(app, &graph.continuation, content.width as usize)
                            {
                                prompt_focus = Some(items.len());
                                targets.push((i, None));
                                items.push(ListItem::new(line));
                            }
                        }
                    }
                } else {
                    targets.push((i, None));
                    items.push(ListItem::new(format!(
                        "{}  {}",
                        graph.continuation,
                        editor.error.as_deref().unwrap_or("Loading hunks…")
                    )));
                }
            }
            if let Some(row) = prompt_focus {
                selected_row = Some(row);
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
            .block(block)
            .highlight_style(Style::default().bg(Color::Indexed(236)))
            .highlight_symbol(""),
        area,
        &mut app.graph_selection,
    );
    if let Some((row, column)) = bookmark_cursor {
        let offset = app.graph_selection.offset();
        if row >= offset
            && row - offset < content.height as usize
            && column < content.width as usize
        {
            frame.set_cursor_position((
                content.x + column as u16,
                content.y + (row - offset) as u16,
            ));
        }
    }
    for (row, (revision, file)) in targets
        .into_iter()
        .skip(app.graph_selection.offset())
        .take(content.height as usize)
        .enumerate()
    {
        if let Some((index, offset)) = chunk_rows.get(&(row + app.graph_selection.offset())) {
            app.chunk_hit_rows.push((
                ratatui::layout::Rect::new(content.x, content.y + row as u16, content.width, 1),
                *index,
                *offset,
            ));
        }
        if let Some(index) = operation_rows.get(&(row + app.graph_selection.offset())) {
            app.operation_hit_rows.push((
                ratatui::layout::Rect::new(content.x, content.y + row as u16, content.width, 1),
                *index,
            ));
        }
        app.hit_rows.push((
            ratatui::layout::Rect::new(content.x, content.y + row as u16, content.width, 1),
            revision,
            file,
        ));
    }
}

fn draw_text(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    text: &str,
    scroll: &mut u16,
    diff: bool,
) {
    let block = Block::default();
    let content = block.inner(area);
    let count = text.lines().count();
    *scroll = (*scroll as usize)
        .min(count.saturating_sub(content.height as usize))
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
        Paragraph::new(lines).scroll((*scroll, 0)).block(block),
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

fn draw_bookmarks(frame: &mut ratatui::Frame, body: ratatui::layout::Rect, app: &mut App) {
    let width = body.width.saturating_sub(4).min(90).max(body.width.min(4));
    let height = body
        .height
        .saturating_sub(4)
        .min(24)
        .max(body.height.min(4));
    let popup = ratatui::layout::Rect::new(
        body.x + (body.width - width) / 2,
        body.y + (body.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(ratatui::widgets::Clear, popup);
    let block = Block::bordered().title(if app.view == View::Git {
        " Git Operations "
    } else {
        " Bookmark Operations "
    });
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let [remote_area, filter_area, list_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Min(1),
    ])
    .areas(inner);
    let mut remotes = vec![Span::styled(
        "Remotes: ",
        Style::default().fg(Color::DarkGray),
    )];
    for (index, name) in app.bookmark_remotes.iter().enumerate() {
        remotes.push(Span::styled(
            format!(" {name} "),
            if index == app.bookmark_remote {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(remotes)), remote_area);
    let filter = if let Some(input) = &app.input
        && input.action != "command"
    {
        input.visible()
    } else {
        format!(
            "Showing: {}{}",
            if app.bookmark_category.is_empty() {
                "all actions"
            } else {
                &app.bookmark_category
            },
            if app.bookmark_filter.is_empty() {
                String::new()
            } else {
                format!(" · {}", app.bookmark_filter)
            }
        )
    };
    frame.render_widget(Paragraph::new(filter), filter_area);
    let rows = app.listing_rows();
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(if app.busy {
                "Loading…"
            } else {
                if app.view == View::Git {
                    "No matching Git operations · a adds a remote"
                } else {
                    "No matching bookmark operations"
                }
            })
            .style(Style::default().fg(Color::DarkGray)),
            list_area,
        );
        return;
    }
    let items = rows
        .iter()
        .map(|row| {
            let command = row
                .bookmark
                .as_ref()
                .map(|action| crate::commands::CommandSpec::new(action.args.clone()).display())
                .unwrap_or_default();
            ListItem::new(vec![
                Line::raw(row.label.clone()),
                Line::styled(command, Style::default().fg(Color::DarkGray)),
                Line::raw(""),
            ])
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default().with_selected(Some(app.browse_index.min(items.len() - 1)));
    frame.render_stateful_widget(
        List::new(items)
            .highlight_style(Style::default().bg(Color::DarkGray))
            .highlight_symbol(""),
        list_area,
        &mut state,
    );
    for (visible, index) in (state.offset()..app.listing_rows().len()).enumerate() {
        let y = list_area.y + (visible * 3) as u16;
        if y >= list_area.bottom() {
            break;
        }
        app.operation_hit_rows.push((
            ratatui::layout::Rect::new(
                list_area.x,
                y,
                list_area.width,
                3.min(list_area.bottom() - y),
            ),
            index,
        ));
    }
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
    fn content_starts_below_header_and_preserves_stale_warning() {
        let mut app = key_fixture();
        rendered_rows(&mut app, 40);
        assert_eq!(app.hit_rows.first().unwrap().0.y, 1);
        app.snapshot.as_mut().unwrap().workspace_stale = true;
        let rows = rendered_rows(&mut app, 40);
        assert!(rows[1].contains("Working copy STALE"));
        assert_eq!(app.hit_rows.first().unwrap().0.y, 2);
        app.snapshot.as_mut().unwrap().workspace_stale = false;
        app.view = View::Text;
        app.output = "command output".into();
        assert!(rendered_rows(&mut app, 40)[1].starts_with("command output"));
    }

    #[test]
    fn version_indicator_is_at_the_right_of_the_header() {
        let mut app = key_fixture();
        app.jj_version = version::Status::Installed(version::library_version().into());
        let expected = app.jj_version.label().0;
        let rows = rendered_rows(&mut app, 40);
        assert!(rows[0].starts_with(" jjui-rs "));
        assert!(rows[0].ends_with(&expected));
    }

    #[test]
    fn description_editing_reuses_its_row_and_puts_context_only_in_footer() {
        let mut app = key_fixture();
        rendered_rows(&mut app, 40);
        let positions = app.hit_rows.clone();
        press(&mut app, KeyCode::Char('i'));
        let input = app.input.as_mut().unwrap();
        input.text = "edited description".into();
        input.cursor = input.text.len();
        let rows = rendered_rows(&mut app, 40);
        assert_eq!(app.hit_rows, positions);
        assert_eq!(
            rows.iter()
                .filter(|line| line.contains("edited description▏"))
                .count(),
            1
        );
        assert!(!rows[..38].iter().any(|line| line.contains("Description")
            || line.contains("apply")
            || line.contains("cancel")));
        assert!(rows[38].contains("enter apply") && rows[38].contains("esc cancel"));
        assert!(rows[39].contains("Description"));
    }

    #[test]
    fn shared_prompts_and_destination_selection_do_not_repeat_footer_hints() {
        let mut app = key_fixture();
        for action in ["describe", "revset", "split", "plugin_prompt"] {
            app.start_input(action, "Field context", "value".into(), false);
            let lines = prompt_lines(&app, "│ ", 80);
            assert_eq!(lines.len(), 1);
            assert!(lines[0].to_string().ends_with("value▏"));
            assert_eq!(footer_action_status(&app).as_deref(), Some("Field context"));
        }
        app.input = None;
        rendered_rows(&mut app, 40);
        let positions = app.hit_rows.clone();
        press(&mut app, KeyCode::Char('S'));
        let rows = rendered_rows(&mut app, 40);
        assert_eq!(app.hit_rows, positions);
        assert!(rows[39].contains("Squash 1 revision(s)"));
        assert!(
            !rows[..38]
                .iter()
                .any(|line| line.contains("apply") || line.contains("cancel"))
        );
    }

    #[test]
    fn undo_confirmation_is_modal_and_does_not_shift_the_graph() {
        let mut app = key_fixture();
        rendered_rows(&mut app, 40);
        let positions = app.hit_rows.clone();
        assert_eq!(press(&mut app, KeyCode::Char('u')), Effect::None);
        let rows = rendered_rows(&mut app, 40);
        let title_row = rows
            .iter()
            .position(|row| row.contains("Confirm undo"))
            .unwrap();
        assert!((15..25).contains(&title_row));
        assert!(
            rows.iter()
                .any(|row| row.contains("enter confirm") && row.contains("esc cancel"))
        );
        assert!(!rows.iter().any(|row| row.contains("[undo]")));
        assert_eq!(app.hit_rows, positions);
        assert!(!app.pending_command);
        assert_eq!(press(&mut app, KeyCode::Esc), Effect::None);
        assert!(app.input.is_none());
        assert!(!app.pending_command);
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Effect::Request(crate::live::Request::Command(
                crate::commands::CommandSpec::new(["undo"])
            ))
        );
    }

    #[test]
    fn tags_are_visible_and_searchable_even_in_immutable_history() {
        let mut app = key_fixture();
        let mut snapshot = app.full_snapshot.clone().unwrap();
        snapshot.revisions[1].tags = vec!["0.1.0".into()];
        snapshot.revisions[1].immutable = true;
        app.update(snapshot);
        assert!(!app.hidden_immutable.contains_key("parent"));
        assert!(
            rendered_rows(&mut app, 30)
                .iter()
                .any(|row| row.contains("0.1.0"))
        );
        let revision = &app.snapshot.as_ref().unwrap().revisions[1];
        let header = revision_header(revision, "○ ", false, None);
        assert!(
            header
                .spans
                .iter()
                .any(|span| span.content == "0.1.0 " && span.style.fg == Some(Color::Magenta))
        );
        app.filter = "0.1.0".into();
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.selected_id(), Some("parent"));
    }

    #[test]
    fn omitted_ancestors_expand_temporarily_and_survive_refresh() {
        let snapshot = Snapshot {
            workspace_stale: false,
            operation: "op".into(),
            revisions: vec![Revision {
                working_copy: true,
                log_context: true,
                parents: vec!["a".into()],
                ..revision("work", "work")
            }],
            ancestors: vec![
                Revision {
                    outside_revset: true,
                    immutable: true,
                    bookmarks: vec!["old@origin".into()],
                    parents: vec!["between".into(), "side".into()],
                    description: "omitted ancestor".into(),
                    ..revision("a", "alpha")
                },
                Revision {
                    outside_revset: true,
                    immutable: true,
                    parents: vec!["older".into()],
                    description: "between bookmarks".into(),
                    ..revision("between", "between")
                },
                Revision {
                    outside_revset: true,
                    immutable: true,
                    elide_bookmarks: true,
                    bookmarks: vec!["older@origin".into()],
                    parents: vec!["root".into()],
                    ..revision("older", "older")
                },
                Revision {
                    outside_revset: true,
                    immutable: true,
                    root: true,
                    ..revision("root", "root")
                },
                Revision {
                    outside_revset: true,
                    immutable: true,
                    parents: vec!["root".into()],
                    ..revision("side", "side")
                },
            ],
        };
        let mut app = App::default();
        let mut snapshot = snapshot;
        snapshot.ancestors[1].parents = vec!["between-two".into()];
        snapshot.ancestors.insert(
            2,
            Revision {
                outside_revset: true,
                immutable: true,
                parents: vec!["older".into()],
                ..revision("between-two", "between-two")
            },
        );
        app.update(snapshot.clone());
        let rows = rendered_rows(&mut app, 30);
        // The collapsed graph is exactly the selected log plus its existing
        // boundary, even when omitted history contains branches and merges.
        let expected_graph = crate::graph::render(&snapshot.revisions);
        assert_eq!(app.graph.len(), expected_graph.len() + 1);
        for (actual, expected) in app.graph.iter().zip(&expected_graph) {
            assert_eq!(actual.node, expected.node);
            assert_eq!(actual.edge, expected.edge);
            assert!(actual.boundary.is_none());
        }
        assert_eq!(
            app.graph.last().unwrap().node,
            *expected_graph.last().unwrap().boundary.as_ref().unwrap()
        );
        assert!(!rows.iter().any(|row| row.contains("ancestor revisions")));
        assert!(!rows.iter().any(|row| row.contains("old@origin")));
        assert!(
            app.snapshot
                .as_ref()
                .unwrap()
                .revisions
                .iter()
                .all(|r| { revision_graph_style(r).fg == Some(Color::Reset) })
        );
        press(&mut app, KeyCode::Char('j'));
        assert!(app.selected_elision().is_some());
        assert!(
            footer_hints(&app, 200)
                .to_string()
                .contains("enter show remote ancestry")
        );
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.selected_id(), Some("a"));
        for revision in &app.snapshot.as_ref().unwrap().revisions {
            assert_eq!(
                revision_graph_style(revision).fg,
                Some(if revision.commit_id == "work" {
                    Color::Reset
                } else {
                    Color::DarkGray
                })
            );
        }
        assert!(
            rendered_rows(&mut app, 30)
                .iter()
                .any(|row| row.contains("old@origin"))
        );
        let rows = rendered_rows(&mut app, 30);
        assert!(rows.iter().any(|row| row.contains("older@origin")));
        assert!(rows.iter().any(|row| row.contains("2 immutable revisions")));
        assert!(!app.hidden_immutable.contains_key("side"));
        assert!(
            !rows
                .iter()
                .any(|row| row.contains("~ 1 immutable revision"))
        );
        assert!(!rows.iter().any(|row| row.contains("between bookmarks")));
        press(&mut app, KeyCode::Char('j'));
        assert!(app.selected_elision().is_some());
        assert!(app.ancestors_expanded);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.selected_id(), Some("between"));
        press(&mut app, KeyCode::Esc);
        assert!(app.selected_elision().is_some());
        assert!(app.ancestors_expanded);
        press(&mut app, KeyCode::Char('j'));
        app.update(snapshot.clone());
        assert_eq!(app.selected_id(), Some("older"));
        assert!(app.hidden_immutable.contains_key("between"));
        press(&mut app, KeyCode::Esc);
        assert!(app.selected_elision().is_some());
        assert!(app.expanded_immutable.is_empty());
        assert!(!app.ancestors_expanded);
        assert!(
            app.snapshot
                .as_ref()
                .unwrap()
                .revisions
                .iter()
                .all(|r| { revision_graph_style(r).fg == Some(Color::Reset) })
        );
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.selected_id(), Some("work"));
        assert!(app.expanded_immutable.is_empty());
        assert!(!app.ancestors_expanded);
        assert_eq!(app.snapshot.as_ref().unwrap().revisions.len(), 2);
    }

    #[test]
    fn immutable_rows_expand_in_place_and_remain_expanded_after_refresh() {
        let snapshot = Snapshot {
            ancestors: Vec::new(),
            workspace_stale: false,
            operation: "op".into(),
            revisions: vec![
                Revision {
                    working_copy: true,
                    parents: vec!["a".into()],
                    ..revision("work", "work")
                },
                Revision {
                    immutable: true,
                    parents: vec!["b".into()],
                    description: "hidden newest".into(),
                    ..revision("a", "alpha")
                },
                Revision {
                    immutable: true,
                    parents: vec!["root".into()],
                    description: "hidden oldest".into(),
                    ..revision("b", "beta")
                },
                Revision {
                    immutable: true,
                    root: true,
                    ..revision("root", "root")
                },
            ],
        };
        let mut app = App::default();
        app.update(snapshot.clone());
        assert_eq!(app.snapshot.as_ref().unwrap().revisions.len(), 3);
        let rows = rendered_rows(&mut app, 30);
        assert!(
            app.snapshot
                .as_ref()
                .unwrap()
                .revisions
                .iter()
                .all(|r| { revision_graph_style(r).fg == Some(Color::Reset) })
        );
        assert!(
            rows.iter()
                .any(|row| row.contains("~ 2 immutable revisions"))
        );
        assert!(!rows.iter().any(|row| row.contains("hidden oldest")));
        press(&mut app, KeyCode::Char('j'));
        assert!(app.selected_id().is_none());
        assert!(
            footer_hints(&app, 200)
                .to_string()
                .contains("enter show hidden commits")
        );
        assert_eq!(press(&mut app, KeyCode::Char('a')), Effect::None);
        assert!(app.input.is_none());
        assert_eq!(press(&mut app, KeyCode::Enter), Effect::None);
        assert!(app.input.is_none());
        assert_eq!(app.selected_id(), Some("a"));
        assert_eq!(app.snapshot.as_ref().unwrap().revisions.len(), 4);
        press(&mut app, KeyCode::Char('j'));
        app.update(snapshot);
        assert_eq!(app.selected_id(), Some("b"));
        assert!(app.hidden_immutable.is_empty());
        assert!(
            rendered_rows(&mut app, 30)
                .iter()
                .any(|row| row.contains("hidden oldest"))
        );
        press(&mut app, KeyCode::Esc);
        assert!(app.selected_elision().is_some());
        assert!(app.expanded_immutable.is_empty());
        assert_eq!(app.selection.selected(), Some(1));
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected_id(), Some("b"));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected_id(), Some("root"));
        assert!(app.expanded_immutable.is_empty());
        assert_eq!(app.snapshot.as_ref().unwrap().revisions.len(), 3);
        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.selected_id(), Some("work"));
        assert!(app.expanded_immutable.is_empty());
    }

    #[test]
    fn folded_rebase_destination_expands_before_submitting_and_supports_custom_keys() {
        let mut app = key_fixture();
        let mut snapshot = app.full_snapshot.clone().unwrap();
        snapshot.revisions[1].immutable = true;
        snapshot.revisions[1].parents = vec![snapshot.revisions[2].commit_id.clone()];
        snapshot.revisions[2].immutable = true;
        app.update(snapshot);
        press(&mut app, KeyCode::Char('r'));
        press(&mut app, KeyCode::Char('j'));
        assert!(app.selected_elision().is_some());
        app.keys.merge("[elision]\nexpand=['x']\n").unwrap();
        assert_eq!(press(&mut app, KeyCode::Enter), Effect::None);
        assert!(app.selected_elision().is_some());
        assert_eq!(press(&mut app, KeyCode::Char('x')), Effect::None);
        assert!(app.selected_elision().is_none());
        assert!(app.target.is_some());
        assert!(!app.pending_command);
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
        let line = revision_header(&revision, "○ ", false, None);
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
        revision.divergent = true;
        let line = revision_header(&revision, "@ ", false, None);
        let marker = line
            .spans
            .iter()
            .find(|span| span.content == " (divergent)")
            .unwrap();
        assert_eq!(marker.style.fg, Some(Color::LightRed));
        revision.working_copy = false;
        let line = revision_header(&revision, "○ ", false, None);
        let marker = line
            .spans
            .iter()
            .find(|span| span.content == " (divergent)")
            .unwrap();
        assert_eq!(marker.style.fg, Some(Color::Red));
        assert!(line.to_string().ends_with("03a6cacb (divergent)"));
        revision.divergent = false;
        revision.working_copy = true;
        revision.empty = true;
        let line = revision_header(&revision, "@ ", false, None);
        assert!(line.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(
            line.spans
                .iter()
                .find(|s| s.content == "yq")
                .unwrap()
                .style
                .fg,
            Some(Color::LightMagenta)
        );
        assert_eq!(
            line.spans
                .iter()
                .find(|s| s.content == "@")
                .unwrap()
                .style
                .fg,
            Some(Color::Green)
        );
        assert_eq!(
            revision_description(&revision, "│ ").to_string(),
            "│ (empty) (no description set)"
        );
        revision.root = true;
        assert_eq!(
            revision_header(&revision, "◆ ", false, None).to_string(),
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
            ancestors: Vec::new(),
            workspace_stale: false,
            operation: "1".into(),
            revisions: vec![revision("a", "x"), revision("b", "y")],
        });
        app.selection.select(Some(1));
        app.update(Snapshot {
            ancestors: Vec::new(),
            workspace_stale: false,
            operation: "2".into(),
            revisions: vec![revision("c", "y"), revision("a", "x")],
        });
        assert_eq!(app.selection.selected(), Some(0));
        app.update(Snapshot {
            ancestors: Vec::new(),
            workspace_stale: false,
            operation: "3".into(),
            revisions: vec![revision("a", "x")],
        });
        assert_eq!(app.selection.selected(), Some(0));
        app.filter = "no match".into();
        app.update(Snapshot {
            ancestors: Vec::new(),
            workspace_stale: false,
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
            ancestors: Vec::new(),
            workspace_stale: false,
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
    fn navigation_uses_vim_keys_and_modifiers() {
        let mut app = key_fixture();
        press(&mut app, KeyCode::Char('J'));
        assert_eq!(app.selection.selected(), Some(1));
        press(&mut app, KeyCode::Char('K'));
        assert_eq!(app.selection.selected(), Some(0));
        press(&mut app, KeyCode::PageDown);
        assert_eq!(app.selection.selected(), Some(2));
        assert_eq!(press(&mut app, KeyCode::Char('g')), Effect::None);
        assert!(matches!(
            press(&mut app, KeyCode::Char('i')),
            Effect::Request(_)
        ));
        assert_eq!(app.selection.selected(), Some(2));
        app.busy = false;
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('@'));
        assert_eq!(app.selection.selected(), Some(0));
        assert_eq!(press(&mut app, KeyCode::Char('r')), Effect::None);
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL), 2),
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
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.selection.selected(), Some(2));
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.selection.selected(), Some(0));
        press(&mut app, KeyCode::Char('N'));
        assert_eq!(app.selection.selected(), Some(2));
        press(&mut app, KeyCode::Esc);
        assert!(app.filter.is_empty());
        assert_eq!(app.selection.selected(), Some(2));
    }

    #[test]
    fn vim_motions_search_direction_and_quit_commands() {
        let mut app = key_fixture();
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(app.selection.selected(), Some(2));
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.selection.selected(), Some(2));
        press(&mut app, KeyCode::Esc);
        assert!(app.key_sequence.is_empty());
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.selection.selected(), Some(0));
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('r'));
        assert!(
            app.target.is_none(),
            "an invalid sequence must not start rebase"
        );
        press(&mut app, KeyCode::Char('v'));
        assert!(app.marked.contains("child"));
        app.filter = "match".into();
        press(&mut app, KeyCode::Char('?'));
        press(&mut app, KeyCode::Enter);
        assert!(app.search_backwards);
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.selection.selected(), Some(2));
        press(&mut app, KeyCode::Char('N'));
        assert_eq!(app.selection.selected(), Some(0));
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL), 4);
        assert_eq!(app.input.as_ref().unwrap().action, "redo");
        press(&mut app, KeyCode::Esc);
        app.view = View::Text;
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL), 4);
        assert_eq!(app.scroll, 4);
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL), 4);
        assert_eq!(app.scroll, 12);
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL), 4);
        assert_eq!(app.scroll, 8);
        app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL), 4);
        assert_eq!(app.scroll, 0);
        press(&mut app, KeyCode::Char(':'));
        press(&mut app, KeyCode::Char('q'));
        assert_eq!(press(&mut app, KeyCode::Enter), Effect::Quit);
        press(&mut app, KeyCode::Char('Z'));
        assert_eq!(press(&mut app, KeyCode::Char('Q')), Effect::Quit);
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

    fn highlighted_rows(app: &mut App, height: u16) -> Vec<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .chunks(120)
            .filter(|row| row.iter().any(|cell| cell.bg != Color::Reset))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect()
    }

    fn operation_fixture(count: usize) -> Vec<crate::browser::Row> {
        (0..count)
            .map(|i| crate::browser::Row {
                bookmark: None,
                id: format!("{i:012x}"),
                label: format!("operation-{i}"),
                operation: Some(crate::operations::Entry {
                    prefix_len: 12,
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
            let lines = highlighted_rows(&mut app, 14);
            assert!(lines.iter().any(|line| line.contains("000000000027")));
            press(&mut app, KeyCode::Home);
            let lines = highlighted_rows(&mut app, 14);
            assert!(lines.iter().any(|line| line.contains("000000000000")));
        }
    }

    #[test]
    fn operation_ids_highlight_only_the_unique_prefix() {
        let mut rows = operation_fixture(1);
        rows[0].operation.as_mut().unwrap().prefix_len = 2;
        let graph = crate::operations::graph(&rows);
        let lines = operation_lines(&rows[0], &graph[0], "");
        let prefix = lines[0].spans.iter().find(|s| s.content == "00").unwrap();
        let suffix = lines[0]
            .spans
            .iter()
            .find(|s| s.content == "0000000000")
            .unwrap();
        assert_eq!(prefix.style.fg, Some(Color::LightBlue));
        assert!(prefix.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(suffix.style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn action_instructions_use_footers_without_shifting_graph_rows() {
        let mut app = key_fixture();
        let before = rendered_rows(&mut app, 40);
        let parent_before = before
            .iter()
            .position(|line| line.contains("other") && line.contains("parent"))
            .unwrap();
        press(&mut app, KeyCode::Char('B'));
        let rows = rendered_rows(&mut app, 40);
        let prompt = rows
            .iter()
            .position(|line| line.contains("match-on") && line.contains('▏'))
            .unwrap();
        let parent = rows
            .iter()
            .position(|line| line.contains("other") && line.contains("parent"))
            .unwrap();
        assert!(prompt < parent && parent == parent_before);
        assert!(!rows.iter().any(|line| line.contains("Set bookmark")));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('r'));
        press(&mut app, KeyCode::Char('j'));
        let rows = rendered_rows(&mut app, 40);
        assert!(
            rows.iter()
                .any(|line| line.contains("match-on") && line.contains("[rebase source]"))
        );
        assert!(
            rows.iter()
                .any(|line| line.contains("other") && line.contains("[rebase destination]"))
        );
        assert!(
            rows.last()
                .unwrap()
                .contains("Rebase 1 revision(s) → other")
        );
        assert!(rows[rows.len() - 2].contains("enter apply"));
        assert_eq!(
            rows.iter()
                .position(|line| line.contains("other") && line.contains("parent")),
            Some(parent_before)
        );
        assert!(
            !rows[..rows.len() - 2]
                .iter()
                .any(|line| line.contains("choose destination") || line.contains("apply · Esc"))
        );
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('a'));
        let rows = rendered_rows(&mut app, 40);
        let prompt = rows
            .iter()
            .position(|line| line.contains("Confirm abandon"))
            .unwrap();
        let next = rows
            .iter()
            .position(|line| line.contains("match-tw"))
            .unwrap();
        assert_eq!(prompt, rows.len() - 1);
        assert_eq!(
            Some(next),
            before.iter().position(|line| line.contains("match-tw"))
        );
        assert!(rows[rows.len() - 2].contains("enter confirm"));
    }

    #[test]
    fn colon_input_occupies_bottom_status_line_in_every_view() {
        for view in [
            View::Revisions,
            View::Files,
            View::Bookmarks,
            View::Git,
            View::Operations,
            View::Help,
        ] {
            let mut app = key_fixture();
            app.view = view;
            press(&mut app, KeyCode::Char(':'));
            for c in "bookmark list".chars() {
                press(&mut app, KeyCode::Char(c));
            }
            let rows = rendered_rows(&mut app, 40);
            assert!(rows.last().unwrap().starts_with(":bookmark list▏"));
            assert!(
                !rows[..rows.len() - 1]
                    .iter()
                    .any(|line| line.contains("bookmark list"))
            );
            assert!(!rows.iter().any(|line| line.contains("Command (:q")));
            press(&mut app, KeyCode::Esc);
            let rows = rendered_rows(&mut app, 40);
            assert!(rows.last().unwrap().contains("revset:"));
        }
    }

    #[test]
    fn git_menu_opens_with_gi_and_filters_commands() {
        let mut app = key_fixture();
        assert_eq!(press(&mut app, KeyCode::Char('g')), Effect::None);
        assert!(matches!(
            press(&mut app, KeyCode::Char('i')),
            Effect::Request(crate::live::Request::Listing(
                crate::browser::Query::GitOperations(_)
            ))
        ));
        app.busy = false;
        app.bookmark_remotes = vec!["origin".into()];
        app.listing = ["fetch", "push"]
            .into_iter()
            .map(|kind| crate::browser::Row {
                id: kind.into(),
                label: format!("{kind} origin"),
                operation: None,
                bookmark: Some(crate::browser::BookmarkAction {
                    kind: kind.into(),
                    name: "origin".into(),
                    remote: Some("origin".into()),
                    local: false,
                    args: vec![
                        "git".into(),
                        kind.into(),
                        "--remote".into(),
                        "origin".into(),
                    ],
                }),
            })
            .collect();
        let rows = rendered_rows(&mut app, 40);
        assert!(rows.iter().any(|line| line.contains("Git Operations")));
        assert!(rows.iter().any(|line| line.contains("match-on")));
        press(&mut app, KeyCode::Char('f'));
        assert!(app.input.is_none());
        assert_eq!(app.listing_rows().len(), 1);
        let Effect::Request(crate::live::Request::Command(spec)) = press(&mut app, KeyCode::Enter)
        else {
            panic!("expected fetch");
        };
        assert_eq!(spec.args, ["git", "fetch", "--remote", "origin"]);
    }

    #[test]
    fn bookmark_popup_filters_applies_and_keeps_graph_visible() {
        let mut app = key_fixture();
        press(&mut app, KeyCode::Char('B'));
        assert_eq!(app.view, View::Revisions);
        assert_eq!(app.input.as_ref().unwrap().action, "set_bookmark");
        press(&mut app, KeyCode::Esc);
        let effect = press(&mut app, KeyCode::Char('b'));
        assert!(matches!(
            effect,
            Effect::Request(crate::live::Request::Listing(
                crate::browser::Query::BookmarkOperations(_)
            ))
        ));
        app.busy = false;
        app.bookmark_remotes = vec!["local".into(), "origin".into()];
        app.listing = [
            ("move", "topic", None, true),
            ("delete", "topic", None, true),
            ("untrack", "topic", Some("origin"), true),
            ("track", "remote-only", Some("origin"), false),
        ]
        .into_iter()
        .map(|(kind, name, remote, local)| crate::browser::Row {
            id: format!("{kind}:{name}"),
            label: format!("{kind} '{name}'"),
            operation: None,
            bookmark: Some(crate::browser::BookmarkAction {
                kind: kind.into(),
                name: name.into(),
                remote: remote.map(str::to_owned),
                local,
                args: vec!["bookmark".into(), kind.into(), name.into()],
            }),
        })
        .collect();
        let rows = rendered_rows(&mut app, 40);
        assert!(rows.iter().any(|row| row.contains("Bookmark Operations")));
        assert!(
            rows.iter().any(|row| row.contains("match-tw")),
            "graph remains behind the popup"
        );
        assert_eq!(app.listing_rows().len(), 3);
        press(&mut app, KeyCode::Char('d'));
        assert!(
            app.input.is_none(),
            "d filters; it does not open a confirmation"
        );
        assert_eq!(app.listing_rows().len(), 1);
        assert_eq!(
            app.listing_rows()[0].bookmark.as_ref().unwrap().kind,
            "delete"
        );
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.view, View::Bookmarks);
        assert!(app.bookmark_category.is_empty());
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.listing_rows().len(), 2);
        press(&mut app, KeyCode::Char('/'));
        for key in "remote-only".chars() {
            press(&mut app, KeyCode::Char(key));
        }
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.listing_rows().len(), 1);
        let effect = press(&mut app, KeyCode::Enter);
        let Effect::Request(crate::live::Request::Command(command)) = effect else {
            panic!("expected bookmark command");
        };
        assert_eq!(command.args, ["bookmark", "track", "remote-only"]);
        assert!(
            matches!(press(&mut app, KeyCode::Enter), Effect::None),
            "no duplicate mutations"
        );
        app.pending_command = false;
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.view, View::Bookmarks);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.view, View::Revisions);
    }

    #[test]
    fn footer_hints_follow_pending_sequences_and_custom_continuations() {
        let mut app = key_fixture();
        press(&mut app, KeyCode::Char('g'));
        let hints = footer_hints(&app, 200).to_string();
        for expected in [
            "g first revision",
            "i Git",
            "n new",
            "v evolution",
            "t reload theme",
            "? help",
            "esc cancel",
        ] {
            assert!(hints.contains(expected), "{expected}: {hints}");
        }
        assert!(!hints.contains("l files"));
        press(&mut app, KeyCode::Esc);
        assert!(footer_hints(&app, 200).to_string().contains("l files"));
        app.keys
            .merge("[revisions]\nnew=['z x n', 'z x m']\nevolution=[]")
            .unwrap();
        press(&mut app, KeyCode::Char('z'));
        assert!(footer_hints(&app, 200).to_string().contains("xn new"));
        press(&mut app, KeyCode::Char('x'));
        let hints = footer_hints(&app, 200).to_string();
        assert!(hints.contains("n new") && hints.contains("m new"));
        assert!(!hints.contains("Git"));
        for width in [1, 10, 40] {
            assert!(footer_hints(&app, width).width() <= width);
        }
        press(&mut app, KeyCode::Char('!'));
        assert!(footer_hints(&app, 200).to_string().contains("l files"));
        app.view = View::Files;
        app.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL), 2);
        assert!(
            footer_hints(&app, 200)
                .to_string()
                .contains("w switch focus")
        );
        press(&mut app, KeyCode::Char('w'));
        assert!(
            !footer_hints(&app, 200)
                .to_string()
                .contains("w switch focus")
        );
        press(&mut app, KeyCode::Char('Z'));
        assert!(footer_hints(&app, 200).to_string().contains("Q quit"));
    }

    #[test]
    fn footer_hints_follow_mode_overrides_and_prompt_state() {
        let mut app = key_fixture();
        assert!(footer_hints(&app, 200).to_string().contains("l files"));
        app.view = View::Files;
        let files = footer_hints(&app, 200).to_string();
        assert!(files.contains("i hunks") && files.contains("r restore"));
        assert!(!files.contains("describe"));
        app.view = View::Chunks;
        assert!(!footer_hints(&app, 200).to_string().contains("restore"));
        for key in ['r', 's'] {
            assert!(matches!(press(&mut app, KeyCode::Char(key)), Effect::None));
            assert!(app.input.is_none());
        }
        app.view = View::Operations;
        app.listing = operation_fixture(1);
        assert!(
            !footer_hints(&app, 200)
                .to_string()
                .contains("snapshot diff")
        );
        app.listing[0].operation.as_mut().unwrap().snapshot = true;
        app.keys
            .merge("[operations]\ndiff=['x']\noperation_restore=[]")
            .unwrap();
        let hints = footer_hints(&app, 200);
        assert!(hints.to_string().contains("x snapshot diff"));
        assert!(!hints.to_string().contains("restore operation"));
        let key = hints.spans.iter().find(|span| span.content == "x").unwrap();
        assert_eq!(key.style.fg, Some(Color::LightCyan));
        assert!(key.style.add_modifier.contains(Modifier::BOLD));
        let action = hints
            .spans
            .iter()
            .find(|span| span.content == " snapshot diff")
            .unwrap();
        assert_eq!(action.style.fg, Some(Color::DarkGray));
        assert!(!action.style.add_modifier.contains(Modifier::BOLD));
        app.start_input("abandon", "Confirm abandon", String::new(), true);
        let hints = footer_hints(&app, 200).to_string();
        assert!(hints.contains("enter confirm") && hints.contains("esc cancel"));
        assert!(!hints.contains("snapshot diff"));
        app.input = None;
        app.target = Some(("rebase".into(), vec!["child".into()], vec![]));
        assert!(footer_hints(&app, 200).to_string().contains("destination"));
        for width in [1, 12, 40, 80] {
            assert!(footer_hints(&app, width).width() <= width);
        }
    }

    #[test]
    fn inline_hunks_capture_selection_and_confirmation() {
        let mut app = key_fixture();
        app.view = View::Chunks;
        app.chunks = Some(ChunkEditor {
            line_offset: 0,
            id: "child".into(),
            path: "file.txt".into(),
            index: 0,
            selected: Default::default(),
            error: None,
            session: Some(crate::chunks::Session {
                commit_id: "child".into(),
                change_id: "match-one".into(),
                path: "file.txt".into(),
                before: Some(crate::chunks::TextFile {
                    text: "old\nkeep\nold2".into(),
                    executable: false,
                }),
                after: Some(crate::chunks::TextFile {
                    text: "new\nkeep\nnew2".into(),
                    executable: false,
                }),
            }),
        });
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('v'));
        assert_eq!(app.chunks.as_ref().unwrap().selected, [1].into());
        let rows = rendered_rows(&mut app, 40);
        assert!(rows.iter().any(|line| line.contains("Hunks · file.txt")));
        assert!(rows.iter().any(|line| line.contains("[x]")));
        press(&mut app, KeyCode::Char('r'));
        assert!(app.input.as_ref().unwrap().confirm);
        assert!(
            rendered_rows(&mut app, 40)
                .iter()
                .any(|line| line.contains("Restore 1 selected hunk"))
        );
        let effect = press(&mut app, KeyCode::Enter);
        let Effect::Request(crate::live::Request::ApplyChunks(request)) = effect else {
            panic!("expected hunk application");
        };
        assert_eq!(request.selected, [1].into());
        assert_eq!(request.action, crate::chunks::Action::Restore);
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
            assert!(rows[1].starts_with("--- file"));
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
        assert!(!expanded.iter().any(|line| line.contains('▸')));
        assert!(
            highlighted_rows(&mut app, 30)
                .iter()
                .any(|line| line.contains("changed.txt"))
        );
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
            highlighted_rows(&mut app, 12)
                .iter()
                .any(|line| line.contains("file-39.txt"))
        );
        press(&mut app, KeyCode::Left);
        let collapsed = rendered_rows(&mut app, 12);
        assert!(
            collapsed
                .iter()
                .any(|line| line.contains("match-on") && line.contains("child"))
        );
        assert!(
            highlighted_rows(&mut app, 12)
                .iter()
                .any(|line| line.contains("child"))
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
    fn plugin_prompt_cancellation_resumes_callback_with_original_context() {
        let mut app = key_fixture();
        app.start_plugin("plugin.example");
        let context = app.plugin_call.take().unwrap().context;
        app.plugin_actions
            .push_back(crate::plugins::Action::Prompt {
                title: "Example".into(),
                callback: "answered".into(),
            });
        assert!(app.next_plugin_effect().is_none());
        assert_eq!(press(&mut app, KeyCode::Esc), Effect::None);
        assert!(app.input.is_none());
        let call = app.plugin_call.take().unwrap();
        assert_eq!(call.function, "answered");
        assert!(call.response.cancelled);
        assert_eq!(call.context, context);
        assert!(!app.pending_command);
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
