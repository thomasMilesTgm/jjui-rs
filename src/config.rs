use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    pub auto_snapshot: bool,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
struct Overlay {
    auto_snapshot: Option<bool>,
}

/// Keep CLI overrides separate so switching workspaces can load its own settings.
#[derive(Clone, Debug, Default)]
pub struct Source {
    pub path: Option<PathBuf>,
    pub auto_snapshot: Option<bool>,
}
impl Source {
    pub fn load(&self, workspace: &Path) -> Result<Config> {
        let home = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
        self.load_with_home(workspace, home.as_deref())
    }

    fn load_with_home(&self, workspace: &Path, home: Option<&Path>) -> Result<Config> {
        let mut config = Config::default();
        if let Some(path) = &self.path {
            config.overlay(path)?;
        } else {
            if let Some(home) = home {
                let path = home.join("jjui-rs/config.toml");
                if path.exists() {
                    config.overlay(&path)?;
                }
            }
            let path = workspace
                .canonicalize()
                .context("Cannot open configuration workspace")?;
            let root = path
                .ancestors()
                .find(|p| p.join(".jj").is_dir())
                .unwrap_or(&path);
            let path = root.join("config.toml");
            if path.exists() {
                config.overlay(&path)?;
            }
        }
        if let Some(enabled) = self.auto_snapshot {
            config.auto_snapshot = enabled;
        }
        Ok(config)
    }
}
impl Config {
    fn overlay(&mut self, path: &Path) -> Result<()> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("Read config {}", path.display()))?;
        let overlay: Overlay = toml::from_str(&contents)
            .with_context(|| format!("Invalid config {}", path.display()))?;
        if let Some(enabled) = overlay.auto_snapshot {
            self.auto_snapshot = enabled;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layers_preserve_omitted_values_and_cli_overrides_follow_workspace_switches() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("config");
        std::fs::create_dir_all(home.join("jjui-rs")).unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir_all(first.join(".jj")).unwrap();
        std::fs::create_dir_all(first.join("subdir")).unwrap();
        std::fs::create_dir_all(second.join(".jj")).unwrap();
        let mut source = Source::default();
        assert!(
            !source
                .load_with_home(&first, Some(&home))
                .unwrap()
                .auto_snapshot
        );
        std::fs::write(home.join("jjui-rs/config.toml"), "auto-snapshot = true").unwrap();
        std::fs::write(first.join("config.toml"), "# no override").unwrap();
        assert!(
            source
                .load_with_home(&first.join("subdir"), Some(&home))
                .unwrap()
                .auto_snapshot
        );
        std::fs::write(second.join("config.toml"), "auto-snapshot = false").unwrap();
        assert!(
            !source
                .load_with_home(&second, Some(&home))
                .unwrap()
                .auto_snapshot
        );
        source.auto_snapshot = Some(true);
        assert!(
            source
                .load_with_home(&second, Some(&home))
                .unwrap()
                .auto_snapshot
        );
        source.auto_snapshot = Some(false);
        assert!(
            !source
                .load_with_home(&first, Some(&home))
                .unwrap()
                .auto_snapshot
        );
        source.auto_snapshot = None;
        source.path = Some(first.join("config.toml"));
        assert!(
            !source
                .load_with_home(&second, Some(&home))
                .unwrap()
                .auto_snapshot
        );
        std::fs::write(first.join("config.toml"), "auto-snapshot = 'yes'").unwrap();
        assert!(source.load_with_home(&first, Some(&home)).is_err());
        std::fs::write(first.join("config.toml"), "auto-snapshots = true").unwrap();
        assert!(source.load_with_home(&first, Some(&home)).is_err());
        source.path = Some(first.join("missing.toml"));
        assert!(source.load_with_home(&first, Some(&home)).is_err());
    }
}
