use super::*;

#[derive(Default)]
pub(super) struct Menu {
    pub entries: Vec<crate::workspaces::Entry>,
    pub index: usize,
    pub pending: Option<crate::commands::CommandSpec>,
}

impl App {
    pub(super) fn workspace_key(&mut self, key: KeyEvent, page_size: usize) -> Effect {
        let Some(action) = self.binding_action("workspaces", key) else {
            return Effect::None;
        };
        if action == "quit" {
            return Effect::Quit;
        }
        if action == "cancel" {
            self.workspace_menu = None;
            return Effect::None;
        }
        if self.pending_command || self.plugin_session.is_some() {
            self.notice = Some("An action is still running".into());
            return Effect::None;
        }
        if action.starts_with("plugin.") {
            self.workspace_menu = None;
            self.start_plugin(&action);
            return Effect::None;
        }
        let menu = self.workspace_menu.as_mut().unwrap();
        let selected = menu.entries.get(menu.index).cloned();
        match action.as_str() {
            "down" => menu.index = (menu.index + 1).min(menu.entries.len().saturating_sub(1)),
            "up" => menu.index = menu.index.saturating_sub(1),
            "page_down" => {
                menu.index = (menu.index + page_size.saturating_mul(2))
                    .min(menu.entries.len().saturating_sub(1))
            }
            "page_up" => menu.index = menu.index.saturating_sub(page_size.saturating_mul(2)),
            "first" => menu.index = 0,
            "last" => menu.index = menu.entries.len().saturating_sub(1),
            "refresh" => return Effect::Request(crate::live::Request::Workspaces),
            "theme_reload" => return Effect::ReloadTheme,
            "help" => {
                self.workspace_menu = None;
                self.view = View::Help;
                self.scroll = 0;
            }
            "command" => {
                self.workspace_menu = None;
                self.start_input("command", "jj command", String::new(), false);
            }
            "workspace_add" => {
                self.start_input(
                    "workspace_add",
                    "New workspace path (parent: focused revision)",
                    String::new(),
                    false,
                );
                let parent = self.selected_id().map(str::to_owned);
                self.input.as_mut().unwrap().sources = parent.into_iter().collect();
            }
            "workspace_switch" => {
                if let Some(entry) = selected {
                    if entry.current {
                        self.workspace_menu = None;
                    } else if entry.available {
                        return Effect::SwitchWorkspace(entry.path.unwrap());
                    } else {
                        self.error = Some("Workspace path is unknown or unavailable".into());
                    }
                }
            }
            "workspace_rename" | "workspace_update" => {
                if let Some(entry) = selected {
                    if !entry.current {
                        self.notice = Some("Switch to this workspace first".into());
                    } else if action == "workspace_rename" {
                        self.start_input(
                            "workspace_rename",
                            "Rename current workspace",
                            entry.name,
                            false,
                        );
                    } else {
                        return self.command_effect(crate::commands::CommandSpec::new([
                            "workspace",
                            "update-stale",
                        ]));
                    }
                }
            }
            "workspace_forget" | "workspace_remove" => {
                if let Some(entry) = selected {
                    if entry.current {
                        self.error = Some(
                            "Switch to another workspace before forgetting or removing this one"
                                .into(),
                        );
                    } else {
                        let remove = action == "workspace_remove";
                        menu.pending = Some(crate::commands::CommandSpec::new([
                            "workspace",
                            if remove { "remove" } else { "forget" },
                            "--",
                            &entry.name,
                        ]));
                        let label = if remove {
                            format!(
                                "Remove workspace {} and DELETE its directory (including ignored files)?",
                                entry.name
                            )
                        } else {
                            format!("Forget workspace {}? Files stay on disk", entry.name)
                        };
                        self.start_input("workspace_confirm", &label, String::new(), true);
                    }
                }
            }
            _ => {}
        }
        Effect::None
    }

    pub(super) fn workspace_submit(&mut self, input: crate::input::Input) -> Effect {
        use crate::commands::CommandSpec;
        let text = input.text.trim();
        let spec = match input.action.as_str() {
            "workspace_confirm" => self.workspace_menu.as_mut().and_then(|m| m.pending.take()),
            "workspace_add" if !text.is_empty() => {
                let mut args = vec!["workspace".into(), "add".into()];
                if let Some(parent) = input.sources.first() {
                    args.extend(["-r".into(), parent.clone()]);
                }
                args.extend(["--".into(), text.into()]);
                Some(CommandSpec::new(args))
            }
            "workspace_rename" if !text.is_empty() => {
                Some(CommandSpec::new(["workspace", "rename", "--", text]))
            }
            _ => {
                self.error = Some("Enter a workspace path or name".into());
                None
            }
        };
        spec.map_or(Effect::None, |spec| self.command_effect(spec))
    }
}

pub(super) fn draw(frame: &mut ratatui::Frame, app: &mut App) {
    let menu = app.workspace_menu.as_ref().unwrap();
    let area = frame.area();
    let width = area.width.saturating_sub(4).min(120);
    let height = area.height.saturating_sub(6).min(24);
    let popup = ratatui::layout::Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(ratatui::widgets::Clear, popup);
    let block = Block::bordered().title(" Workspaces ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let parts = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(if app.input.is_some() { 2 } else { 0 }),
    ])
    .split(inner);
    let items: Vec<_> = menu
        .entries
        .iter()
        .map(|entry| {
            let name = format!(
                "{}{}",
                entry.name,
                if entry.current { " (current)" } else { "" }
            );
            let name = format!(
                "{name}{}",
                if entry.stale == Some(true) {
                    " [stale]"
                } else {
                    ""
                }
            );
            let path = entry
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "path unknown".into());
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(
                        name,
                        Style::default()
                            .fg(Color::Magenta)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!("  {:.12}  {}", entry.commit, entry.description)),
                ]),
                Line::styled(
                    format!(
                        "  {path}{}",
                        if entry.available {
                            ""
                        } else {
                            " (unavailable)"
                        }
                    ),
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(menu.index));
    frame.render_stateful_widget(
        List::new(items)
            .highlight_symbol("▸ ")
            .highlight_style(Style::default().bg(Color::DarkGray)),
        parts[0],
        &mut state,
    );
    if let Some(input) = &app.input {
        frame.render_widget(
            Paragraph::new(if input.confirm {
                input.label.clone()
            } else {
                input.visible()
            })
            .style(Style::default().fg(Color::Yellow))
            .wrap(ratatui::widgets::Wrap { trim: false }),
            parts[1],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(app: &mut App, key: KeyCode) -> Effect {
        app.handle_key(KeyEvent::new(key, KeyModifiers::NONE), 10)
    }
    fn menu(current: bool, available: bool) -> App {
        App {
            workspace_menu: Some(Menu {
                entries: vec![crate::workspaces::Entry {
                    name: "-literal".into(),
                    path: Some("/other".into()),
                    current,
                    available,
                    stale: None,
                    commit: "0123456789abcdef".into(),
                    description: "example".into(),
                }],
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn workspace_actions_confirm_literal_names_and_protect_current_workspace() {
        let mut app = menu(false, true);
        press(&mut app, KeyCode::Char('d'));
        assert!(app.input.as_ref().unwrap().confirm);
        assert!(footer_hints(&app, 200).to_string().contains("enter apply"));
        assert_eq!(press(&mut app, KeyCode::Esc), Effect::None);
        assert!(app.workspace_menu.as_ref().unwrap().pending.is_none());
        assert!(!app.pending_command);
        press(&mut app, KeyCode::Char('f'));
        let effect = press(&mut app, KeyCode::Enter);
        assert_eq!(
            effect,
            Effect::Request(crate::live::Request::Command(
                crate::commands::CommandSpec::new(["workspace", "forget", "--", "-literal"])
            ))
        );
        let mut app = menu(true, true);
        press(&mut app, KeyCode::Char('d'));
        assert!(app.input.is_none());
        assert!(app.error.as_deref().unwrap().contains("Switch"));
    }

    #[test]
    fn switching_rejects_missing_paths_and_busy_commands() {
        let mut app = menu(false, true);
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Effect::SwitchWorkspace("/other".into())
        );
        app.pending_command = true;
        assert_eq!(press(&mut app, KeyCode::Enter), Effect::None);
        let mut app = menu(false, false);
        assert_eq!(press(&mut app, KeyCode::Enter), Effect::None);
        assert!(app.error.as_deref().unwrap().contains("unavailable"));
    }

    #[test]
    fn menu_opens_via_configured_binding_and_renders_on_small_terminals() {
        let mut app = App::default();
        app.keys.merge("[revisions]\nworkspaces=['w']\n").unwrap();
        assert_eq!(
            press(&mut app, KeyCode::Char('w')),
            Effect::Request(crate::live::Request::Workspaces)
        );
        let mut app = menu(false, true);
        for (width, height) in [(1, 1), (10, 4), (80, 24)] {
            let backend = ratatui::backend::TestBackend::new(width, height);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| super::super::draw(frame, &mut app))
                .unwrap();
        }
    }
}
