# jjui-rs

A Rust terminal UI for [Jujutsu](https://jj-vcs.dev/), inspired by
[jjui](https://github.com/idursun/jjui), with a live commit graph that never calls `jj log` to
refresh.

## Install

Requires Rust 1.97.1+ and `jj` 0.47:

```sh
cargo install jjui-rs
jjui-rs -R /path/to/workspace
```

From a source checkout, use `cargo install --path . --locked`.

The major/minor version follows `jj-lib`; jjui-rs patch releases are independent.

### Update

```sh
cargo install jjui-rs --force
```

- Rebuilds with the latest compatible `jj-lib` patch, even if jjui-rs hasn't had a new release.
- Add `--locked` to use the dependencies tested with the published release; newer dependency patches
  then require a new jjui-rs release with an updated lockfile.
- The header shows the installed `jj` version when it matches, an orange update prompt for patch
  differences, and a red prompt for major/minor differences. A new major/minor requires a matching
  jjui-rs release.

### Debian and Ubuntu packaging

Debian packages and a signed APT feed are being prepared for amd64 and arm64. See the
[setup and release instructions](https://github.com/thomasMilesTgm/jjui-rs/blob/main/docs/debian.md).

## Why another jj UI?

Direct repository access keeps history current without spawning a command for each refresh.

|                  | jjui                     | jjui-rs                                                |
| ---------------- | ------------------------ | ------------------------------------------------------ |
| Implementation   | Go                       | Rust, using `jj-lib`                                   |
| Live refresh     | Polls `jj log` with `-p` | Watches filesystem changes and checks operation heads  |
| Default bindings | jjui bindings            | Vim-oriented, with an optional jjui preset             |
| Compatibility    | Established feature set  | Early implementation; Roto plugins, not Lua-compatible |

The TUI snapshots the active workspace once on opening or switching workspaces, through `jj-lib`.
Enable ongoing snapshots in `config.toml` or with `--auto-snapshot`. Stale workspaces open for
explicit recovery. Editing and Git commands still use the `jj` CLI.

## Features

- The graph follows Jujutsu's `revsets.log`. Its ancestor boundary opens bookmark-level history,
  with immutable stretches expandable separately. Use `-r 'all()'` for full history.
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

- Defaults to `false`; controls ongoing snapshots in the active workspace, after the opening
  snapshot.
- `--once`, `--diff`, and `--watch-json` remain read-only unless automatic snapshotting is enabled.
- Workspace-root `config.toml` overrides global settings; switching workspaces reloads settings.
- `--auto-snapshot` / `--no-auto-snapshot` override the configured value.
- `--config PATH` selects a file instead of the usual layers; `--check-config` validates it.
- See the
  [example configuration](https://github.com/thomasMilesTgm/jjui-rs/blob/main/examples/config.toml).

### Keybindings

- Customize `keybinds.toml`; see the
  [default bindings](https://github.com/thomasMilesTgm/jjui-rs/blob/main/keybinds/keybinds.default.toml).
- For jjui muscle memory: `--keybinds /path/to/keybinds/keybinds.jjui.toml`, using the
  [jjui preset](https://github.com/thomasMilesTgm/jjui-rs/blob/main/keybinds/keybinds.jjui.toml).
  Its comments document compatibility gaps.

### Themes

- Select `--theme dark`, `--theme light`, or `--theme /path/to/theme.toml`.
- Without an explicit selection, global and workspace `theme.toml` files apply.
- See the [example theme](https://github.com/thomasMilesTgm/jjui-rs/blob/main/themes/example.toml);
  validate with `--check-theme`.

### Plugins

- Define custom actions in Roto; see the
  [plugin API and examples](https://github.com/thomasMilesTgm/jjui-rs/blob/main/docs/plugins.md).
- Load a manifest with `--plugins /path/to/plugins.toml`; validate with `--check-plugins`.

## License

Licensed under either [MIT](https://github.com/thomasMilesTgm/jjui-rs/blob/main/LICENSE-MIT) or
[Apache-2.0](https://github.com/thomasMilesTgm/jjui-rs/blob/main/LICENSE-APACHE), at your option.
