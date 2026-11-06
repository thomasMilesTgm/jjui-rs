use anyhow::{Context, Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

type Tables = BTreeMap<String, BTreeMap<String, Vec<String>>>;
#[derive(Debug, PartialEq)]
pub enum BindingMatch {
    Action(String),
    Prefix,
    None,
}
#[derive(Clone, Debug)]
pub struct Keybinds {
    tables: Tables,
}
impl Default for Keybinds {
    fn default() -> Self {
        Self {
            tables: toml::from_str(include_str!("default_keybinds.toml"))
                .expect("valid built-in keys"),
        }
    }
}
impl Keybinds {
    pub fn load(repository: &Path, explicit: Option<&Path>) -> Result<Self> {
        let mut keys = Self::default();
        if let Some(path) = explicit {
            keys.overlay(path)?;
            return Ok(keys);
        }
        let config_home = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")));
        if let Some(home) = config_home {
            let path = home.join("jjui-rs/keybinds.toml");
            if path.exists() {
                keys.overlay(&path)?;
            }
        }
        let path = repository.canonicalize()?;
        let root = path
            .ancestors()
            .find(|p| p.join(".jj").is_dir())
            .unwrap_or(&path);
        let path = root.join("keybinds.toml");
        if path.exists() {
            keys.overlay(&path)?;
        }
        Ok(keys)
    }
    fn overlay(&mut self, path: &Path) -> Result<()> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("Read {}", path.display()))?;
        self.merge(&text)
            .with_context(|| format!("Invalid key bindings in {}", path.display()))
    }
    pub fn merge(&mut self, text: &str) -> Result<()> {
        let overrides: Tables = toml::from_str(text)?;
        let mut tables = self.tables.clone();
        for (scope, actions) in overrides {
            let target = tables
                .get_mut(&scope)
                .with_context(|| format!("Unknown scope {scope}"))?;
            for (action, keys) in actions {
                if !target.contains_key(&action) {
                    bail!("Unknown action {scope}.{action}");
                }
                for key in &keys {
                    let sequence = parse_sequence(key)?;
                    if scope == "input" && sequence.len() != 1 {
                        bail!("Input bindings must be a single key");
                    }
                }
                target.insert(action, keys);
            }
        }
        for (scope, actions) in &tables {
            let mut used = Vec::new();
            for (action, keys) in actions {
                for key in keys {
                    let event = parse_sequence(key)?;
                    if let Some((_, other)) = used.iter().find(|(k, _)| *k == event) {
                        bail!("Key {key:?} in [{scope}] is bound to both {other} and {action}");
                    }
                    used.push((event, action));
                }
            }
            if scope != "global"
                && let Some(global) = tables.get("global")
            {
                for (action, keys) in global {
                    for key in keys {
                        let sequence = parse_sequence(key)?;
                        if !used.iter().any(|(keys, _)| *keys == sequence) {
                            used.push((sequence, action));
                        }
                    }
                }
            }
            for (i, (a, action)) in used.iter().enumerate() {
                for (b, other) in used.iter().skip(i + 1) {
                    if a.starts_with(b) || b.starts_with(a) {
                        bail!(
                            "Ambiguous key sequence prefix in [{scope}] between {action} and {other}"
                        );
                    }
                }
            }
        }
        self.tables = tables;
        Ok(())
    }
    pub fn resolve(&self, scope: &str, key: KeyEvent) -> Option<String> {
        match self.resolve_sequence(scope, &[key]) {
            BindingMatch::Action(action) => Some(action),
            _ => None,
        }
    }
    pub fn resolve_sequence(&self, scope: &str, keys: &[KeyEvent]) -> BindingMatch {
        let keys: Vec<_> = keys.iter().copied().map(normalize).collect();
        let mut prefix = false;
        for scope in [scope, "global"] {
            if let Some(actions) = self.tables.get(scope) {
                for (action, bindings) in actions {
                    for binding in bindings {
                        let sequence = parse_sequence(binding).expect("validated binding");
                        if sequence == keys {
                            return BindingMatch::Action(action.clone());
                        }
                        prefix |= sequence.starts_with(&keys);
                    }
                }
            }
        }
        if prefix {
            BindingMatch::Prefix
        } else {
            BindingMatch::None
        }
    }
    pub fn help(&self) -> String {
        self.tables
            .iter()
            .map(|(scope, actions)| {
                format!(
                    "[{scope}]\n{}",
                    actions
                        .iter()
                        .filter(|(_, keys)| !keys.is_empty())
                        .map(|(action, keys)| format!("  {:26} {}", keys.join(", "), action))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }
    pub fn label(&self, scope: &str, action: &str) -> String {
        self.tables
            .get(scope)
            .and_then(|s| s.get(action))
            .or_else(|| self.tables.get("global").and_then(|s| s.get(action)))
            .filter(|keys| !keys.is_empty())
            .map(|keys| keys.join("/"))
            .unwrap_or_else(|| "unbound".into())
    }
    pub fn hint(&self, scope: &str, action: &str) -> Option<String> {
        self.tables
            .get(scope)
            .and_then(|s| s.get(action))
            .or_else(|| self.tables.get("global").and_then(|s| s.get(action)))
            .and_then(|keys| keys.first())
            .cloned()
    }
}
fn parse_sequence(text: &str) -> Result<Vec<KeyEvent>> {
    let keys = text
        .split_whitespace()
        .map(parse)
        .collect::<Result<Vec<_>>>()?;
    if keys.is_empty() {
        bail!("Empty key sequence; use 'space' for a space key");
    }
    Ok(keys)
}
fn normalize(mut key: KeyEvent) -> KeyEvent {
    key.kind = crossterm::event::KeyEventKind::Press;
    key.state = crossterm::event::KeyEventState::NONE;
    if let KeyCode::Char(c) = key.code {
        key.code = KeyCode::Char(if key.modifiers.contains(KeyModifiers::SHIFT) {
            c.to_ascii_uppercase()
        } else {
            c
        });
        key.modifiers.remove(KeyModifiers::SHIFT);
    }
    key
}
fn parse(text: &str) -> Result<KeyEvent> {
    let mut modifiers = KeyModifiers::NONE;
    let mut rest = text;
    loop {
        if let Some(key) = rest.strip_prefix("ctrl+") {
            modifiers |= KeyModifiers::CONTROL;
            rest = key;
        } else if let Some(key) = rest.strip_prefix("alt+") {
            modifiers |= KeyModifiers::ALT;
            rest = key;
        } else if let Some(key) = rest.strip_prefix("shift+") {
            modifiers |= KeyModifiers::SHIFT;
            rest = key;
        } else {
            break;
        }
    }
    let code = match rest {
        "space" => KeyCode::Char(' '),
        "enter" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backspace" => KeyCode::Backspace,
        "delete" => KeyCode::Delete,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        s if s.chars().count() == 1 => KeyCode::Char(s.chars().next().unwrap()),
        s if s.starts_with('f') => {
            let n = s[1..].parse::<u8>().context("Expected f1..f12")?;
            if !(1..=12).contains(&n) {
                bail!("Expected f1..f12");
            }
            KeyCode::F(n)
        }
        _ => bail!("Unknown key {text:?}"),
    };
    Ok(normalize(KeyEvent::new(code, modifiers)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overrides_disable_old_keys_and_support_modifiers() {
        let mut keys = Keybinds::default();
        keys.merge("[revisions]\ndown=['ctrl+j']\nnew=[]").unwrap();
        assert_eq!(keys.resolve("revisions", parse("j").unwrap()), None);
        assert_eq!(
            keys.resolve("revisions", parse("ctrl+j").unwrap())
                .as_deref(),
            Some("down")
        );
        assert_eq!(
            keys.resolve_sequence("revisions", &parse_sequence("g n").unwrap()),
            BindingMatch::None
        );
        assert_eq!(
            keys.resolve("revisions", parse("n").unwrap()).as_deref(),
            Some("next_match")
        );
        assert_eq!(
            keys.resolve("revisions", parse("shift+j").unwrap())
                .as_deref(),
            Some("parent")
        );
    }
    #[test]
    fn vim_sequences_validate_and_can_be_remapped() {
        let mut keys = Keybinds::default();
        keys.merge("").unwrap();
        assert_eq!(
            keys.resolve_sequence("revisions", &parse_sequence("g").unwrap()),
            BindingMatch::Prefix
        );
        assert_eq!(
            keys.resolve_sequence("revisions", &parse_sequence("g g").unwrap()),
            BindingMatch::Action("first".into())
        );
        assert_eq!(
            keys.resolve_sequence("files", &parse_sequence("ctrl+w w").unwrap()),
            BindingMatch::Action("focus".into())
        );
        assert!(keys.merge("[revisions]\nnew=['g']").is_err());
        assert!(keys.merge("[input]\nsubmit=['g g']").is_err());
        keys.merge("[revisions]\nfirst=['z z']").unwrap();
        assert_eq!(
            keys.resolve_sequence("revisions", &parse_sequence("g g").unwrap()),
            BindingMatch::None
        );
        assert_eq!(
            keys.resolve_sequence("revisions", &parse_sequence("z z").unwrap()),
            BindingMatch::Action("first".into())
        );
    }
    #[test]
    fn invalid_bindings_are_rejected_transactionally() {
        for text in [
            "[typo]\nup=['a']",
            "[revisions]\ntypo=['a']",
            "[revisions]\nup=['j']",
            "[revisions]\nup=['unknown']",
        ] {
            let mut keys = Keybinds::default();
            assert!(keys.merge(text).is_err());
            assert_eq!(
                keys.resolve("revisions", parse("k").unwrap()).as_deref(),
                Some("up")
            );
        }
    }
}
