# jjui-rs

A Rust terminal UI for [Jujutsu](https://jj-vcs.dev/), inspired by
[jjui](https://github.com/idursun/jjui), with a live commit graph that never calls `jj log` to
refresh.

## Install

Requires Rust 1.97.1+ and `jj` 0.46. From this checkout:

```sh
cargo install --path . --locked
jjui-rs -R /path/to/workspace
```

## Why another jj UI?

Direct repository access keeps history current without spawning a command for each refresh.

|                  | jjui                     | jjui-rs                                                |
| ---------------- | ------------------------ | ------------------------------------------------------ |
| Implementation   | Go                       | Rust, using `jj-lib`                                   |
| Live refresh     | Polls `jj log` with `-p` | Watches filesystem changes and checks operation heads  |
| Default bindings | jjui bindings            | Vim-oriented, with an optional jjui preset             |
| Compatibility    | Established feature set  | Early implementation; Roto plugins, not Lua-compatible |

By default, browsing does not snapshot files or create operations. Automatic snapshotting records
working-tree edits through `jj-lib`; enable it in `config.toml` or with `--auto-snapshot`. Editing
and Git commands still use the `jj` CLI.

## Features

- Expandable immutable-history rows in the live graph.
- Inline files, diffs, operation history, and action prompts.
- Bookmark and Git menus, with local/remote divergence markers.
- Workspace switching, creation, renaming, forgetting, removal, and stale-workspace recovery. Live
  history reflects the shared repository; stale working-copy files are flagged for explicit
  recovery.
- Hunk restore and split, with checks for intervening edits.
- Configurable themes and contextual keybinding hints.

Hunk editing supports UTF-8 regular files up to 256 KiB; binary files, conflicts, and mode changes
require whole-file tools.

## Configuration

Workspace configuration, keybinding, and theme files override global files in `~/.config/jjui-rs/`
(or `$XDG_CONFIG_HOME/jjui-rs/`). Explicit file options bypass these layers.

### Automatic snapshots

Set this in `~/.config/jjui-rs/config.toml` to enable automatic snapshots:

```toml
auto-snapshot = true
```

- Defaults to `false`; applies to the workspace currently open in the TUI.
- Workspace-root `config.toml` overrides global settings; switching workspaces reloads settings.
- `--auto-snapshot` / `--no-auto-snapshot` override the configured value.
- `--config PATH` selects a file instead of the usual layers; `--check-config` validates it.
- See the [example configuration](examples/config.toml).

### Keybindings

- Customize `keybinds.toml`; see the [default bindings](keybinds/keybinds.default.toml).
- For jjui muscle memory: `--keybinds /path/to/keybinds/keybinds.jjui.toml`, using the
  [jjui preset](keybinds/keybinds.jjui.toml). Its comments document compatibility gaps.

### Themes

- Select `--theme dark`, `--theme light`, or `--theme /path/to/theme.toml`.
- Without an explicit selection, global and workspace `theme.toml` files apply.
- See the [example theme](themes/example.toml); validate with `--check-theme`.

### Plugins

- Define custom actions in Roto; see the [plugin API and examples](docs/plugins.md).
- Load a manifest with `--plugins /path/to/plugins.toml`; validate with `--check-plugins`.
