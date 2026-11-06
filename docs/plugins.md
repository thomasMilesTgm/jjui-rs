# Roto plugins

Plugins add named actions using [Roto](https://codeberg.org/NLnetLabs/roto) 0.13.0. They are
separate from jjui's Lua plugins.

## Load and bind

Use the included examples:

```sh
jjui-rs --plugins examples/plugins/plugins.toml --keybinds examples/plugins/keybinds.toml
```

The default manifest is `~/.config/jjui-rs/plugins.toml` (or
`$XDG_CONFIG_HOME/jjui-rs/plugins.toml`). `--plugins` replaces that manifest. Repository manifests
are not loaded automatically.

```toml
# plugins.toml: paths are relative to this file.
[actions.describe]
path = "plugins/describe.roto"
entry = "run" # optional; defaults to run
```

```toml
# keybinds.toml: registered names work in global and view scopes.
[revisions]
"plugin.describe" = ["z d"]
```

- Validate compilation and entrypoint signatures with `--plugins PATH --check-plugins`.
- Validate bindings with `--plugins PATH --keybinds PATH --check-keybinds`.
- Script edits are recompiled on the next invocation; an active workflow keeps its compiled version.
- Manifest or binding changes require a restart.
- Compilation failures appear in the footer; full diagnostics are available in command
  history/output.

Load scripts you trust: Roto compiles native code inside the application. Compilation and execution
run on a dedicated worker, separate from rendering; this is not process isolation.

## Entrypoints and callbacks

Every entrypoint and callback has this signature:

```text
fn run(ctx: Context, result: Response) -> Plan {
    Plan.new().notify("Selected change: " + ctx.change_id())
}
```

`Context` captures the selection when the action starts and stays unchanged throughout its
callbacks. `Response` is empty on entry; callbacks receive command output, a dialog answer, or an
error.

| Context method               | Result                                |
| ---------------------------- | ------------------------------------- |
| `repository()`               | Workspace path                        |
| `change_id()`, `commit_id()` | Focused revision IDs; empty if none   |
| `file()`                     | Focused file path; empty if none      |
| `selected_changes()`         | Marked changes, or the focused change |
| `selected_files()`           | Selected files in file details        |
| `revset()`                   | Active revision expression            |

| Response method | Result                                                      |
| --------------- | ----------------------------------------------------------- |
| `value()`       | Command output, dialog answer, diff text, or applied revset |
| `error()`       | Error message; empty on success                             |
| `cancelled()`   | Whether a dialog was cancelled                              |
| `ok()`          | True when neither failed nor cancelled                      |

## Plans

Start with `Plan.new()` and chain methods. The complete plan is validated before any action is
applied, including command restrictions and callback signatures.

| Method                             | Behavior                                              |
| ---------------------------------- | ----------------------------------------------------- |
| `notify(text)`                     | Set the footer message                                |
| `diff(text)`                       | Display supplied diff text                            |
| `select(id)`                       | Focus a full commit/change ID already in the graph    |
| `read_diff(commit_id, callback)`   | Read a recorded diff through `jj-lib`                 |
| `command(args, callback)`          | Run a noninteractive jj command with an argument list |
| `prompt(title, callback)`          | Ask for text                                          |
| `choose(title, options, callback)` | Open a choice menu                                    |
| `revset(expression, callback)`     | Apply a native revset and refresh the graph           |
| `refresh(callback)`                | Refresh native repository state                       |

Methods with a callback must end the plan. The callback returns the next plan once the operation
finishes. An empty callback name ends the workflow. Return an empty plan to stop without further
work. Check `cancelled()` or `ok()` before acting on a response.

Commands use the existing command runner: arguments are passed directly, without shell
interpolation. The same supported-command restrictions apply, including no `jj log` or
repository/config overrides. Use noninteractive arguments such as `--message=...`; external editors
and shell execution are not exposed by this initial API. Command completion refreshes repository
state before invoking the callback. Only one plugin workflow runs at a time.

## Examples

- [Describe](../examples/plugins/describe.roto): prompt, update a description, and report
  success/error.
- [Inspect](../examples/plugins/inspect.roto): choose between a native diff and the selected change
  ID.
