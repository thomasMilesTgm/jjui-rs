use anyhow::{Context, Result, bail};
use ratatui::{buffer::Buffer, style::Color};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    str::FromStr,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    pub foreground: String,
    pub background: String,
    pub selection: String,
    pub hint_key: String,
    pub hint_action: String,
    pub hint_separator: String,
    pub palette: BTreeMap<String, String>,
}
impl Default for Theme {
    fn default() -> Self {
        Self {
            foreground: "reset".into(),
            background: "reset".into(),
            selection: "#303030".into(),
            hint_key: "light_cyan".into(),
            hint_action: "dark_gray".into(),
            hint_separator: "dark_gray".into(),
            palette: BTreeMap::new(),
        }
    }
}
impl Theme {
    pub fn load(root: &Path, choice: Option<&str>) -> Result<Self> {
        let mut theme = Self::default();
        match choice {
            Some("default") => return Ok(theme),
            Some("dark") => {
                theme.foreground = "#d8dee9".into();
                theme.background = "#20242c".into();
                theme.selection = "#3b4252".into();
                theme.palette.insert("dark_gray".into(), "#8490a4".into());
                return Ok(theme);
            }
            Some("light") => {
                theme.foreground = "#24292f".into();
                theme.background = "#ffffff".into();
                theme.selection = "#dbe7f5".into();
                for (name, value) in [
                    ("black", "#ffffff"),
                    ("dark_gray", "#687078"),
                    ("yellow", "#806000"),
                    ("green", "#17732e"),
                    ("cyan", "#006c80"),
                    ("blue", "#174ea6"),
                    ("magenta", "#8735a1"),
                    ("red", "#b42318"),
                    ("gray", "#484f58"),
                    ("light_green", "#17732e"),
                    ("light_blue", "#174ea6"),
                    ("light_magenta", "#8735a1"),
                    ("light_cyan", "#006c80"),
                    ("light_red", "#b42318"),
                ] {
                    theme.palette.insert(name.into(), value.into());
                }
                return Ok(theme);
            }
            Some(path) => theme.overlay(Path::new(path))?,
            None => {
                let home = std::env::var_os("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .or_else(|| {
                        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
                    });
                if let Some(home) = home {
                    let path = home.join("jjui-rs/theme.toml");
                    if path.exists() {
                        theme.overlay(&path)?;
                    }
                }
                let root = root.canonicalize()?;
                let workspace = root
                    .ancestors()
                    .find(|p| p.join(".jj").is_dir())
                    .unwrap_or(&root);
                let path = workspace.join("theme.toml");
                if path.exists() {
                    theme.overlay(&path)?;
                }
            }
        }
        theme.validate()?;
        Ok(theme)
    }
    fn overlay(&mut self, path: &Path) -> Result<()> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("Read theme {}", path.display()))?;
        let value: toml::Value =
            toml::from_str(&text).with_context(|| format!("Invalid theme {}", path.display()))?;
        let parsed: Theme = value
            .clone()
            .try_into()
            .with_context(|| format!("Invalid theme {}", path.display()))?;
        if value.get("foreground").is_some() {
            self.foreground = parsed.foreground;
        }
        if value.get("background").is_some() {
            self.background = parsed.background;
        }
        if value.get("selection").is_some() {
            self.selection = parsed.selection;
        }
        if value.get("hint_key").is_some() {
            self.hint_key = parsed.hint_key;
        }
        if value.get("hint_action").is_some() {
            self.hint_action = parsed.hint_action;
        }
        if value.get("hint_separator").is_some() {
            self.hint_separator = parsed.hint_separator;
        }
        self.palette.extend(parsed.palette);
        self.validate()
            .with_context(|| format!("Invalid theme {}", path.display()))
    }
    fn validate(&self) -> Result<()> {
        for color in [
            &self.foreground,
            &self.background,
            &self.selection,
            &self.hint_key,
            &self.hint_action,
            &self.hint_separator,
        ] {
            parse(color)?;
        }
        for (key, value) in &self.palette {
            if !PALETTE.iter().any(|(name, _)| key == name) {
                bail!("Unknown palette color {key:?}");
            }
            parse(value)?;
        }
        Ok(())
    }
    pub fn hint_colors(&self) -> (Color, Color, Color) {
        (
            parse(&self.hint_key).unwrap(),
            parse(&self.hint_action).unwrap(),
            parse(&self.hint_separator).unwrap(),
        )
    }
    pub fn apply(&self, buffer: &mut Buffer) {
        let palette: Vec<_> = PALETTE
            .iter()
            .filter_map(|(name, color)| {
                self.palette
                    .get(*name)
                    .map(|value| (*color, parse(value).unwrap()))
            })
            .collect();
        let foreground = parse(&self.foreground).unwrap();
        let background = parse(&self.background).unwrap();
        let selection = parse(&self.selection).unwrap();
        for cell in &mut buffer.content {
            cell.fg = if cell.fg == Color::Reset {
                foreground
            } else {
                palette
                    .iter()
                    .find(|(from, _)| *from == cell.fg)
                    .map_or(cell.fg, |(_, to)| *to)
            };
            cell.bg = if matches!(cell.bg, Color::Indexed(236) | Color::DarkGray) {
                selection
            } else if cell.bg == Color::Reset {
                background
            } else {
                palette
                    .iter()
                    .find(|(from, _)| *from == cell.bg)
                    .map_or(cell.bg, |(_, to)| *to)
            };
        }
    }
}
fn parse(value: &str) -> Result<Color> {
    Color::from_str(value).map_err(|_| {
        anyhow::anyhow!("Invalid color {value:?}; use a color name, #RRGGBB, or an ANSI index")
    })
}
const PALETTE: &[(&str, Color)] = &[
    ("black", Color::Black),
    ("red", Color::Red),
    ("green", Color::Green),
    ("yellow", Color::Yellow),
    ("blue", Color::Blue),
    ("magenta", Color::Magenta),
    ("cyan", Color::Cyan),
    ("gray", Color::Gray),
    ("dark_gray", Color::DarkGray),
    ("light_red", Color::LightRed),
    ("light_green", Color::LightGreen),
    ("light_yellow", Color::LightYellow),
    ("light_blue", Color::LightBlue),
    ("light_magenta", Color::LightMagenta),
    ("light_cyan", Color::LightCyan),
    ("white", Color::White),
];

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overlays_validate_and_preserve_unspecified_colors() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("theme.toml");
        std::fs::write(&path, "foreground='#abcdef'\n[palette]\nred='#123456'\n").unwrap();
        let mut theme = Theme::default();
        theme.overlay(&path).unwrap();
        let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, 2, 1));
        buffer[(0, 0)].fg = Color::Red;
        buffer[(0, 0)].bg = Color::Indexed(236);
        theme.apply(&mut buffer);
        assert_eq!(buffer[(0, 0)].fg, Color::Rgb(0x12, 0x34, 0x56));
        assert_eq!(buffer[(1, 0)].fg, Color::Rgb(0xab, 0xcd, 0xef));
        std::fs::write(&path, "[palette]\ntyop='red'").unwrap();
        assert!(theme.overlay(&path).is_err());
        assert!(Theme::load(temp.path(), Some("light")).is_ok());
    }
}
