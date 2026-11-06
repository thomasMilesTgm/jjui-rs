//! Roto scripts produce typed UI requests; they never access the terminal or repository directly.
use anyhow::{Context as _, Result, bail};
use roto::{
    Runtime, library,
    value::{List, RotoString, Val},
};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub actions: BTreeMap<String, Definition>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub path: PathBuf,
    #[serde(default = "entrypoint")]
    pub entry: String,
}
fn entrypoint() -> String {
    "run".into()
}
impl Config {
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        let default = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .map(|home| home.join("jjui-rs/plugins.toml"));
        let Some(path) = explicit.or(default.as_deref()) else {
            return Ok(Self::default());
        };
        if explicit.is_none() && !path.exists() {
            return Ok(Self::default());
        }
        let mut config: Self = toml::from_str(
            &std::fs::read_to_string(path)
                .with_context(|| format!("Read plugins {}", path.display()))?,
        )?;
        for (name, definition) in &mut config.actions {
            if name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                bail!("Invalid plugin action name {name:?}");
            }
            definition.path = path
                .parent()
                .unwrap_or(Path::new("."))
                .join(&definition.path);
        }
        Ok(config)
    }
    pub fn names(&self) -> Vec<String> {
        self.actions
            .keys()
            .map(|name| format!("plugin.{name}"))
            .collect()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Context {
    pub repository: String,
    pub change: String,
    pub commit: String,
    pub file: String,
    pub changes: Vec<String>,
    pub files: Vec<String>,
    pub revset: String,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Response {
    pub value: String,
    pub error: String,
    pub cancelled: bool,
}
impl Response {
    pub fn result(result: Result<String, String>) -> Self {
        match result {
            Ok(value) => Self {
                value,
                ..Self::default()
            },
            Err(error) => Self {
                error,
                ..Self::default()
            },
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Notify(String),
    ReadDiff {
        commit: String,
        callback: String,
    },
    Diff(String),
    Select(String),
    Command {
        args: Vec<String>,
        callback: String,
    },
    Prompt {
        title: String,
        callback: String,
    },
    Choose {
        title: String,
        options: Vec<String>,
        callback: String,
    },
    Revset {
        expression: String,
        callback: String,
    },
    Refresh {
        callback: String,
    },
}
impl Action {
    pub fn callback(&self) -> Option<&str> {
        match self {
            Self::ReadDiff { callback, .. }
            | Self::Command { callback, .. }
            | Self::Prompt { callback, .. }
            | Self::Choose { callback, .. }
            | Self::Revset { callback, .. }
            | Self::Refresh { callback } => Some(callback),
            _ => None,
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub actions: Vec<Action>,
}
fn strings(values: List<RotoString>) -> Vec<String> {
    values.to_vec().into_iter().map(|v| v.to_string()).collect()
}
fn list(values: &[String]) -> List<RotoString> {
    let list = List::new();
    for value in values {
        list.push(value.clone().into());
    }
    list
}
fn runtime() -> Result<Runtime<roto::runtime::context::NoCtx>> {
    Runtime::from_lib(library! {
        /// Selection captured when the action starts.
        #[clone] type Context = Val<Context>;
        /// Result of a command, dialog, refresh, or revset change.
        #[clone] type Response = Val<Response>;
        /// Ordered UI actions. A callback-producing action must be last.
        #[clone] type Plan = Val<Plan>;
        impl Val<Context> {
            fn repository(c: Val<Context>) -> RotoString { c.repository.clone().into() }
            fn change_id(c: Val<Context>) -> RotoString { c.change.clone().into() }
            fn commit_id(c: Val<Context>) -> RotoString { c.commit.clone().into() }
            fn file(c: Val<Context>) -> RotoString { c.file.clone().into() }
            fn selected_changes(c: Val<Context>) -> List<RotoString> { list(&c.changes) }
            fn selected_files(c: Val<Context>) -> List<RotoString> { list(&c.files) }
            fn revset(c: Val<Context>) -> RotoString { c.revset.clone().into() }
        }
        impl Val<Response> {
            fn value(r: Val<Response>) -> RotoString { r.value.clone().into() }
            fn error(r: Val<Response>) -> RotoString { r.error.clone().into() }
            fn cancelled(r: Val<Response>) -> bool { r.cancelled }
            fn ok(r: Val<Response>) -> bool { r.error.is_empty() && !r.cancelled }
        }
        impl Val<Plan> {
            fn new() -> Val<Plan> { Val(Plan::default()) }
            fn notify(mut p: Val<Plan>, text: RotoString) -> Val<Plan> { p.0.actions.push(Action::Notify(text.to_string())); p }
            fn diff(mut p: Val<Plan>, text: RotoString) -> Val<Plan> { p.0.actions.push(Action::Diff(text.to_string())); p }
            fn select(mut p: Val<Plan>, id: RotoString) -> Val<Plan> { p.0.actions.push(Action::Select(id.to_string())); p }
            fn read_diff(mut p: Val<Plan>, commit: RotoString, callback: RotoString) -> Val<Plan> {
                p.0.actions.push(Action::ReadDiff { commit: commit.to_string(), callback: callback.to_string() }); p
            }
            fn command(mut p: Val<Plan>, args: List<RotoString>, callback: RotoString) -> Val<Plan> {
                p.0.actions.push(Action::Command { args: strings(args), callback: callback.to_string() }); p
            }
            fn prompt(mut p: Val<Plan>, title: RotoString, callback: RotoString) -> Val<Plan> {
                p.0.actions.push(Action::Prompt { title: title.to_string(), callback: callback.to_string() }); p
            }
            fn choose(mut p: Val<Plan>, title: RotoString, options: List<RotoString>, callback: RotoString) -> Val<Plan> {
                p.0.actions.push(Action::Choose { title: title.to_string(), options: strings(options), callback: callback.to_string() }); p
            }
            fn revset(mut p: Val<Plan>, expression: RotoString, callback: RotoString) -> Val<Plan> {
                p.0.actions.push(Action::Revset { expression: expression.to_string(), callback: callback.to_string() }); p
            }
            fn refresh(mut p: Val<Plan>, callback: RotoString) -> Val<Plan> {
                p.0.actions.push(Action::Refresh { callback: callback.to_string() }); p
            }
        }
    }).map_err(|error| anyhow::anyhow!("{error}"))
}

type Signature = fn(Val<Context>, Val<Response>) -> Val<Plan>;
struct Script {
    package: roto::Blob<roto::runtime::context::NoCtx>,
    // Recheck module sources too, so edits to imported helpers invalidate the cache.
    sources: Vec<(PathBuf, Vec<u8>)>,
}
fn sources(path: &Path) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    let mut paths = vec![path.to_owned()];
    fn visit(path: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                if entry.file_name().to_string_lossy().starts_with('.')
                    || matches!(entry.file_name().to_str(), Some("target" | "node_modules"))
                {
                    continue;
                }
                visit(&entry.path(), paths)?;
            } else if entry.path().extension().is_some_and(|ext| ext == "roto") {
                paths.push(entry.path());
            }
        }
        Ok(())
    }
    visit(path.parent().unwrap_or(Path::new(".")), &mut paths)?;
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .map(|path| {
            Ok((
                path.clone(),
                std::fs::read(&path).with_context(|| format!("Read plugin {}", path.display()))?,
            ))
        })
        .collect()
}
struct Engine {
    config: Config,
    runtime: Runtime<roto::runtime::context::NoCtx>,
    scripts: BTreeMap<String, Script>,
}
impl Engine {
    fn new(config: Config) -> Result<Self> {
        Ok(Self {
            config,
            runtime: runtime()?,
            scripts: BTreeMap::new(),
        })
    }
    fn script(&mut self, name: &str, reload: bool) -> Result<&mut Script> {
        let definition = self
            .config
            .actions
            .get(name)
            .with_context(|| format!("Unknown plugin {name}"))?;
        if reload || !self.scripts.contains_key(name) {
            let source = sources(&definition.path)?;
            if self
                .scripts
                .get(name)
                .is_none_or(|script| script.sources != source)
            {
                let mut package = self
                    .runtime
                    .compile(definition.path.as_path())
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                package
                    .get_function::<Signature>(&definition.entry)
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                self.scripts.insert(
                    name.to_owned(),
                    Script {
                        package,
                        sources: source,
                    },
                );
            }
        }
        Ok(self.scripts.get_mut(name).unwrap())
    }
    fn call(&mut self, call: Call) -> Result<Plan> {
        let name = call
            .name
            .strip_prefix("plugin.")
            .context("Invalid plugin action")?;
        let entry = self
            .config
            .actions
            .get(name)
            .context("Unknown plugin")?
            .entry
            .clone();
        let function = if call.function.is_empty() {
            entry.as_str()
        } else {
            &call.function
        };
        let script = self.script(name, call.function.is_empty())?;
        let function = script
            .package
            .get_function::<Signature>(function)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let plan = function.call(Val(call.context), Val(call.response)).0;
        if plan.actions.len() > 128 {
            bail!("Plugin returned more than 128 actions");
        }
        for (index, action) in plan.actions.iter().enumerate() {
            if let Some(callback) = action.callback() {
                if index + 1 != plan.actions.len() {
                    bail!("An asynchronous plugin action must be last; continue in its callback");
                }
                if !callback.is_empty() {
                    script
                        .package
                        .get_function::<Signature>(callback)
                        .map_err(|e| anyhow::anyhow!("{e}"))?;
                }
            }
            if let Action::Command { args, .. } = action {
                crate::commands::CommandSpec::new(args.clone()).validate()?;
            }
            if let Action::Choose { options, .. } = action
                && options.is_empty()
            {
                bail!("Plugin choice must contain options");
            }
        }
        Ok(plan)
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub name: String,
    pub function: String,
    pub context: Context,
    pub response: Response,
}
pub struct Worker {
    pub calls: mpsc::SyncSender<Call>,
    pub results: mpsc::Receiver<Result<Plan, String>>,
}
impl Worker {
    pub fn start(config: Config) -> Result<Self> {
        let (calls, receive) = mpsc::sync_channel(1);
        let (send, results) = mpsc::sync_channel(1);
        let (ready, started) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("roto-plugins".into())
            .spawn(move || {
                let engine = Engine::new(config);
                let mut engine = match engine {
                    Ok(engine) => {
                        let _ = ready.send(Ok(()));
                        engine
                    }
                    Err(error) => {
                        let _ = ready.send(Err(format!("{error:#}")));
                        return;
                    }
                };
                while let Ok(call) = receive.recv() {
                    let result = engine.call(call).map_err(|error| format!("{error:#}"));
                    if send.send(result).is_err() {
                        break;
                    }
                }
            })?;
        started.recv()?.map_err(anyhow::Error::msg)?;
        Ok(Self { calls, results })
    }
}
pub fn check(config: Config) -> Result<Vec<String>> {
    let mut engine = Engine::new(config)?;
    let names: Vec<_> = engine.config.actions.keys().cloned().collect();
    for name in &names {
        engine
            .script(name, true)
            .with_context(|| format!("Plugin {name}"))?;
    }
    Ok(names)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Task {
    ReadDiff(String),
    Command(Vec<String>),
    Revset(String),
    Refresh,
}
#[derive(Clone, Debug)]
pub struct Session {
    pub name: String,
    pub context: Context,
    pub callback: Option<String>,
}
#[derive(Clone, Debug)]
pub struct Choice {
    pub title: String,
    pub options: Vec<String>,
    pub index: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(source: &str) -> (tempfile::TempDir, Engine) {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("action.roto"), source).unwrap();
        std::fs::write(
            temp.path().join("plugins.toml"),
            "[actions.example]\npath='action.roto'\n",
        )
        .unwrap();
        let config = Config::load(Some(&temp.path().join("plugins.toml"))).unwrap();
        (temp, Engine::new(config).unwrap())
    }
    fn call(function: &str, response: Response) -> Call {
        Call {
            name: "plugin.example".into(),
            function: function.into(),
            context: Context {
                change: "selected-change".into(),
                commit: "selected-commit".into(),
                changes: vec!["marked-change".into()],
                files: vec!["a | b.txt".into()],
                ..Default::default()
            },
            response,
        }
    }
    #[test]
    fn typed_callbacks_preserve_arguments_and_reload_between_invocations() {
        let (temp, mut engine) = fixture(
            r#"
            fn run(ctx: Context, result: Response) -> Plan { Plan.new().prompt("Description", "done") }
            fn done(ctx: Context, result: Response) -> Plan {
                if result.cancelled() { Plan.new().notify("cancelled") }
                else { Plan.new().command(["describe", "-r", ctx.change_id(), "--message=" + result.value()], "") }
            }
        "#,
        );
        assert!(
            matches!(&engine.call(call("", Response::default())).unwrap().actions[0], Action::Prompt {callback,..} if callback == "done")
        );
        std::fs::write(temp.path().join("action.roto"), "invalid source").unwrap();
        let response = Response {
            value: "literal ; $(not-a-shell)\nline 2".into(),
            ..Default::default()
        };
        let plan = engine.call(call("done", response)).unwrap();
        let Action::Command { args, .. } = &plan.actions[0] else {
            panic!("expected command");
        };
        assert_eq!(
            args,
            &[
                "describe",
                "-r",
                "selected-change",
                "--message=literal ; $(not-a-shell)\nline 2"
            ]
        );
        assert!(engine.call(call("", Response::default())).is_err());
        std::fs::write(
            temp.path().join("action.roto"),
            "fn run(ctx: Context, result: Response) -> Plan { Plan.new().notify(\"reloaded\") }",
        )
        .unwrap();
        assert_eq!(
            engine.call(call("", Response::default())).unwrap().actions,
            [Action::Notify("reloaded".into())]
        );
    }
    #[test]
    fn validates_callbacks_and_commands_before_applying_any_actions() {
        for source in [
            "fn run(ctx: Context, result: Response) -> Plan { Plan.new().command([\"describe\"], \"missing\") }",
            "fn run(ctx: Context, result: Response) -> Plan { Plan.new().command([\"log\"], \"\") }",
            "fn run(ctx: Context, result: Response) -> Plan { Plan.new().prompt(\"name\", \"\").notify(\"too late\") }",
        ] {
            let (_temp, mut engine) = fixture(source);
            assert!(engine.call(call("", Response::default())).is_err());
        }
    }
    #[test]
    fn example_plugins_compile_and_dialog_cancellation_returns_no_commands() {
        let config = Config::load(Some(Path::new("examples/plugins/plugins.toml"))).unwrap();
        assert_eq!(check(config.clone()).unwrap(), ["describe", "inspect"]);
        let mut engine = Engine::new(config).unwrap();
        let mut request = call("", Response::default());
        request.name = "plugin.inspect".into();
        assert!(matches!(
            engine.call(request.clone()).unwrap().actions[0],
            Action::Choose { .. }
        ));
        request.function = "chosen".into();
        request.response.cancelled = true;
        assert!(engine.call(request).unwrap().actions.is_empty());
    }
    #[test]
    fn bindings_require_registered_names_and_keep_multikey_help() {
        let (temp, engine) =
            fixture("fn run(ctx: Context, result: Response) -> Plan { Plan.new() }");
        let path = temp.path().join("keys.toml");
        std::fs::write(&path, "[revisions]\n'plugin.example'=['z p']\n").unwrap();
        assert!(
            crate::keybinds::Keybinds::load_with_plugins(temp.path(), Some(&path), &[]).is_err()
        );
        let keys = crate::keybinds::Keybinds::load_with_plugins(
            temp.path(),
            Some(&path),
            &engine.config.names(),
        )
        .unwrap();
        assert_eq!(
            keys.plugin_hints("revisions"),
            [("z p".into(), "example".into())]
        );
        assert!(keys.help().contains("plugin.example"));
    }
}
