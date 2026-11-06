use anyhow::{Context, Result, bail};
use jj_lib::config::{self, ConfigLayer, ConfigSource, StackedConfig};
use jj_lib::settings::UserSettings;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

pub fn load(root: &Path) -> Result<UserSettings> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|p| p.join(".config")));
    let mut config = StackedConfig::with_defaults();
    config.add_layer(ConfigLayer::parse(
        ConfigSource::EnvBase,
        r#"
[user]
name = "jjui-rs"
email = ""
[operation]
hostname = "localhost"
username = "jjui-rs"
[snapshot]
auto-track = "all()"
max-new-file-size = "1MiB"
"#,
    )?);
    let paths = if let Some(paths) = std::env::var_os("JJ_CONFIG") {
        std::env::split_paths(&paths)
            .filter(|p| !p.as_os_str().is_empty())
            .collect::<Vec<_>>()
    } else {
        home.as_ref()
            .map(|p| p.join(".jjconfig.toml"))
            .into_iter()
            .chain(config_home.as_ref().map(|p| p.join("jj/config.toml")))
            .collect()
    };
    for path in paths {
        load_path(&mut config, ConfigSource::User, &path)?;
    }
    let repo_path = root.join(".jj/repo");
    let repo_path = if repo_path.is_file() {
        root.join(".jj")
            .join(std::fs::read_to_string(&repo_path)?.trim())
            .canonicalize()?
    } else {
        repo_path
    };
    let repo_path = repo_path.canonicalize()?;
    load_local(
        &mut config,
        ConfigSource::Repo,
        &repo_path,
        "config-id",
        "config.toml",
        config_home.as_deref().map(|p| p.join("jj/repos")),
    )?;
    load_local(
        &mut config,
        ConfigSource::Workspace,
        &root.join(".jj"),
        "workspace-config-id",
        "workspace-config.toml",
        config_home.as_deref().map(|p| p.join("jj/workspaces")),
    )?;
    let mut env = ConfigLayer::empty(ConfigSource::EnvOverrides);
    for (key, name) in [("user.name", "JJ_USER"), ("user.email", "JJ_EMAIL")] {
        if let Ok(value) = std::env::var(name) {
            env.set_value(key, value)?;
        }
    }
    config.add_layer(env);
    let environment: HashMap<_, _> = std::env::vars().collect();
    let config = config::resolve(
        &config,
        &config::ConfigResolutionContext {
            home_dir: home.as_deref(),
            repo_path: Some(&repo_path),
            workspace_path: Some(root),
            command: Some("jjui-rs"),
            hostname: environment
                .get("HOSTNAME")
                .map(String::as_str)
                .unwrap_or("localhost"),
            environment: &environment,
        },
    )?;
    Ok(UserSettings::from_config_and_home_dir(config, home)?)
}
fn load_path(config: &mut StackedConfig, source: ConfigSource, path: &Path) -> Result<()> {
    if path.is_dir() {
        config
            .load_dir(source, path)
            .with_context(|| format!("Load {}", path.display()))?;
    } else if path.is_file() {
        config
            .load_file(source, path)
            .with_context(|| format!("Load {}", path.display()))?;
    }
    Ok(())
}

// SecureConfig::maybe_load_config can migrate or create files. Resolve existing
// secure config read-only and verify its path identity before reading settings.
fn load_local(
    config: &mut StackedConfig,
    source: ConfigSource,
    directory: &Path,
    id_file: &str,
    legacy: &str,
    secure_root: Option<PathBuf>,
) -> Result<()> {
    let id_path = directory.join(id_file);
    if !id_path.exists() {
        return load_path(config, source, &directory.join(legacy));
    }
    let id = std::fs::read_to_string(&id_path)?;
    if id.len() != 20 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("Invalid config ID in {}", id_path.display());
    }
    let Some(secure_root) = secure_root else {
        return Ok(());
    };
    let secure = secure_root.join(id);
    if !secure.exists() {
        return Ok(());
    }
    let metadata = jj_lib::secure_config::read_metadata(&secure)?;
    let expected = directory.canonicalize()?;
    let actual =
        jj_lib::secure_config::metadata_path(&metadata)?.and_then(|p| p.canonicalize().ok());
    if actual.as_deref() != Some(expected.as_path()) {
        bail!(
            "Secure config identity does not match {}; run jj config path to update moved/copied repository metadata",
            directory.display()
        );
    }
    load_path(config, source, &secure.join("config.toml"))
}
