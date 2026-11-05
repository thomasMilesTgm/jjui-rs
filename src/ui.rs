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
    widgets::{Block, List, ListItem, ListState, Paragraph, Wrap},
};
use std::{collections::VecDeque, time::Duration};

#[derive(Default)]
struct App {
    snapshot: Option<Snapshot>,
    selection: ListState,
    files: VecDeque<String>,
    error: Option<String>,
    filter: String,
    searching: bool,
    notice: Option<String>,
}

#[derive(Debug, PartialEq)]
enum Effect {
    None,
    Quit,
    Refresh,
}

impl App {
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

    fn handle_key(&mut self, key: KeyEvent, page_size: usize) -> Effect {
        if key.kind == KeyEventKind::Release {
            return Effect::None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('c') => Effect::Quit,
                KeyCode::Char('r') if !self.searching => Effect::Refresh,
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
        match code {
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
            KeyCode::Char('r') => {
                self.notice = Some("Rebase (r) is not implemented yet. Ctrl-R refreshes.".into())
            }
            KeyCode::Char('g') => {
                self.notice =
                    Some("Git view (g) is not implemented yet. Home/End jump to first/last.".into())
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
        self.selection
            .select((!visible.is_empty()).then_some(selected));
        self.error = None;
    }
}

pub fn run(live: Live) -> Result<()> {
    let mut terminal = ratatui::init();
    // ratatui also installs a panic hook to restore the terminal.
    let result = (|| -> Result<()> {
        let mut app = App::default();
        loop {
            for update in live.updates.try_iter() {
                match update {
                    Update::Repository(snapshot) => app.update(snapshot),
                    Update::Error(error) => app.error = Some(error),
                    Update::Files(paths) => {
                        for path in paths {
                            app.files.retain(|p| p != &path);
                            app.files.push_front(path);
                            app.files.truncate(100);
                        }
                    }
                }
            }
            terminal.draw(|frame| {
                let [header, body, footer] = Layout::vertical([
                    Constraint::Length(2), Constraint::Min(4), Constraint::Length(2),
                ]).areas(frame.area());
                let op = app.snapshot.as_ref().map(|s| short(&s.operation)).unwrap_or("loading");
                frame.render_widget(Paragraph::new(Line::from(vec![
                    Span::styled(" jjui-rs ", Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)),
                    Span::raw(format!("  LIVE · jj-lib · op {op}")),
                ])), header);
                let [history, side] = Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(body);
                let [details, files] = Layout::vertical([Constraint::Percentage(65), Constraint::Percentage(35)]).areas(side);
                let visible = app.visible();
                let matches: Vec<_> = visible.iter().map(|i| app.matches(*i)).collect();
                let items: Vec<_> = visible.iter().map(|i| {
                    let r = &app.snapshot.as_ref().unwrap().revisions[*i];
                    let description = r.description.lines().next().filter(|s| !s.is_empty()).unwrap_or("(no description)");
                    ListItem::new(vec![Line::from(vec![
                        Span::styled(if r.working_copy { "@ " } else { "○ " }, Style::default().fg(Color::Green)),
                        Span::styled(short(&r.change_id), Style::default().fg(Color::Magenta)),
                        Span::raw(format!("  {} ", short(&r.commit_id))),
                        Span::styled(r.bookmarks.join(" "), Style::default().fg(Color::Cyan)),
                    ]), Line::raw(format!("  {description}"))]).style(if matches[*i] { Style::default().fg(Color::Yellow) } else { Style::default() })
                }).collect();
                frame.render_stateful_widget(List::new(items)
                    .block(Block::bordered().title(format!(" Revisions · {} · /{} ", visible.len(), app.filter)))
                    .highlight_style(Style::default().bg(Color::DarkGray)).highlight_symbol("▸ "), history, &mut app.selection);
                let selected = app.selection.selected().and_then(|i| visible.get(i))
                    .map(|i| &app.snapshot.as_ref().unwrap().revisions[*i]);
                let text = selected.map(|r| format!("Change  {}\nCommit  {}\nAuthor  {}\nParents {}\nBookmarks {}\n\n{}",
                    r.change_id, r.commit_id, r.author, r.parents.iter().map(|s| short(s)).collect::<Vec<_>>().join(", "),
                    r.bookmarks.join(", "), r.description)).unwrap_or_else(|| "No matching revisions".into());
                frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }).block(Block::bordered().title(" Revision details ")), details);
                let paths = if app.files.is_empty() { "Waiting for file edits…".into() } else { app.files.iter().cloned().collect::<Vec<_>>().join("\n") };
                frame.render_widget(Paragraph::new(paths).block(Block::bordered().title(" Recent file events · not snapshotted ")), files);
                let help = if let Some(error) = &app.error { error.clone() }
                    else if let Some(notice) = &app.notice { notice.clone() }
                    else if app.searching { format!("Search: /{}  · Enter accept · Esc clear", app.filter) }
                    else { "j/k move · PgUp/PgDn page · @ working copy · J/K parent/child · Ctrl-R refresh · q quit\n/ search · apostrophe/quote next/previous match · Esc clear · Home/End first/last".into() };
                frame.render_widget(Paragraph::new(help).style(Style::default().fg(if app.error.is_some() { Color::Red } else { Color::Gray })), footer);
            })?;
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            let page_size = terminal.size()?.height.saturating_sub(6) as usize / 2;
            match app.handle_key(key, page_size) {
                Effect::Quit => break,
                Effect::Refresh => {
                    let _ = live.refresh.try_send(());
                }
                Effect::None => {}
            }
        }
        Ok(())
    })();
    ratatui::restore();
    result
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
        }
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
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.selection.selected(), Some(2));
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
}
