# jjui-rs

A Rust terminal UI for [Jujutsu](https://jj-vcs.dev/), inspired by
[jjui](https://github.com/idursun/jjui). Browse a live commit graph, expand changed
files and operation history inline, view diffs, and edit changes without leaving
the terminal.

## Install

Requires Rust 1.97.1+ and `jj` 0.46 for editing and Git commands. From this checkout:

```sh
cargo install --path . --locked
jjui-rs -R /path/to/workspace
```

Run `jjui-rs` inside a workspace to open it. Use `:help` for bindings and `:q` to quit.

## Why another jj UI?

The main difference is how it stays live. Rather than repeatedly invoking
`jj log`, jjui-rs reads repository state directly through `jj-lib`, watches for
filesystem changes, and checks operation heads to recover missed events. The aim
is to keep the graph current without spawning a history command for each refresh.
Browsing does not snapshot files or create operations.

To also record working-tree edits automatically and keep their diffs live, run
`jjui-rs --auto-snapshot`. This uses native Jujutsu snapshots; explicit editing,
Git actions, and external editors still use the `jj` CLI.

The interface favors Vim-style keys and inline context: `l` expands files, `o`
expands operations related to the focused change, `O` opens the full operation
log, and `d` shows a diff, including the changes recorded by a snapshot operation.
Bindings are configurable in [keybinds.toml](keybinds.toml), with global overrides
at `~/.config/jjui-rs/keybinds.toml` (or `$XDG_CONFIG_HOME/jjui-rs/keybinds.toml`).

This is an independent, early implementation, not a feature-complete jjui port.
It is tied to `jj-lib` 0.46 and does not yet support jjui's plugins, themes, or
in-TUI chunk editing.
