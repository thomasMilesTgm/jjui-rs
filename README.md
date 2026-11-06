# jjui-rs

A Rust Jujutsu TUI with an always-live commit graph, inline file details, and Vim-style bindings. Graph reads, revsets, diffs, bookmark/file listings, operation history, and automatic snapshotting use `jj-lib`. Graph refresh never runs `jj log`.

```sh
cargo run --release -- -R /path/to/workspace
```

Requires Rust 1.97.1+. The library is pinned to Jujutsu 0.46.0. Install `jj` 0.46 for explicit editing, Git actions, evolution history, and external editors. Browsing and native automatic snapshots work without `jj` on `PATH`.

## Configurable keys

Every keyboard action is configured through `keybinds.toml`. The supplied file lists the defaults. Configuration is layered in this order:

1. Built-in defaults (`src/default_keybinds.toml`).
2. `$XDG_CONFIG_HOME/jjui-rs/keybinds.toml`, or `~/.config/jjui-rs/keybinds.toml`.
3. `keybinds.toml` at the open workspace root.

`--keybinds /path/to/keybinds.toml` uses only that file over the built-in defaults. Each specified action replaces its entire key list; omitted actions retain their previous bindings. Use `[]` to disable an action. Restart after editing the file.

```toml
[revisions]
down = ["ctrl+j", "down"]
up = ["ctrl+k", "up"]
new = ["x"]

[global]
quit = ["ctrl+q"]

[files]
cancel = ["esc", "left"]
```

Scopes are `global`, `revisions`, `files`, `view`, `bookmarks`, `operations`, `finder`, `git`, and `input`. A scope-specific binding wins over a global binding. Keys are case-sensitive; `J` and `shift+j` are equivalent. Named keys include `enter`, `esc`, `space`, `tab`, arrows, `home`, `end`, `pageup`, `pagedown`, `backspace`, `delete`, and `f1`–`f12`. Modifiers can be combined, such as `ctrl+alt+x`.

Unknown scopes/actions/keys, malformed TOML, duplicate keys within a scope, and ambiguous sequence prefixes produce startup errors with the config path. To move a key between actions in one scope, remove it from the previous action too. Text input uses the `input` bindings; typing `q`, `n`, etc. inside an input never triggers revision commands. Help and status hints show effective bindings. Space-separated strings define key sequences: `first = ["g g", "home"]` means press `g` twice, and `focus = ["ctrl+w w", "tab"]` means Ctrl-W followed by `w`. Escape cancels a pending sequence; unmatched sequences are discarded. Prompt bindings must be single keys so typing remains literal.

```sh
cargo run -- --check-keybinds
cargo run -- --keybinds ./custom.toml --check-keybinds
```

## Default workflows

These defaults use Vim navigation and search while keeping domain actions such as `o`/`O` and `d`. `v` toggles marks; this is not a full Vim visual mode or text editor. New changes, Git, and evolution history use `gn`, `gp`, and `gv` to leave `n`/`N`, `gg`, and `v` available for navigation and selection.

| Keys | Action |
| --- | --- |
| `j` / `k`, arrows | Navigate |
| `gg` / `G`, Home/End | First / last item or line |
| Ctrl-D/U, Ctrl-F/B; PgDn/PgUp | Half-page / full-page navigation in the focused pane |
| `@`, `J`, `K` | Working copy, parent, child |
| `/` / `?`, `n` / `N` | Forward / backward graph search; repeat / reverse the search direction |
| `v` / Space | Toggle revision or file marking |
| `l` / Right | Expand files beneath the selected commit, retaining graph edges |
| `h` / Left / Esc | Collapse files or return from a view |
| `d` | Revision diff, or selected-file diff from inline details |
| `p`, `P`, Ctrl-W `w` / Tab | Toggle preview, side/bottom placement, switch focus |
| `L` | Edit revset, evaluated by jj-lib |
| `gn` | New change from the cursor revision or marked revisions |
| `i` / Enter, `D` | Describe inline, or in the configured external editor |
| `e`, `E` | Edit revision, or open configured diff editor |
| `r`, `S` | Rebase/squash: capture sources, navigate to destination, Enter to apply |
| `a`, `A`, `y` | Abandon with confirmation, absorb, duplicate |
| `s` | Split in the configured diff editor; in details, split selected files with an inline description |
| `r` in details | Restore selected files from the revision's merged parents, with confirmation |
| `c` | Commit the current working copy with an inline description |
| `B` | Set/move a named bookmark to the cursor revision |
| `u`, Ctrl-R | Undo/redo with confirmation |
| `o`, `O`, `gv` | Inline operations for the focused change, full operation log, evolution history |
| `d` in operation log | Diff the working-copy changes recorded by a snapshot operation |
| `r` in operation log | Restore the selected operation, with confirmation |
| `b` | Repository-wide local and remote bookmarks |
| `d`, `t`, `u` in bookmarks | Delete local bookmark; track/untrack remote bookmark |
| `gp`, then `f` / `p` | Git remotes; fetch / push using the entered remote |
| `a` / `d` in Git view | Add / remove remote |
| Ctrl-T | Fuzzy file finder over the selected revision's recorded tree; `/` filters and Enter opens content |
| `:`, `$`, `W` | Explicit jj command, shell command, session command history |
| Ctrl-S | Explicit working-copy snapshot |
| Ctrl-L | Refresh |
| `g?` / F1 / `:help` | Effective key bindings and recent file events |
| `:q` / `ZQ` / Ctrl-C | Quit |

In prompts, Enter applies, Esc / Ctrl-[ / Ctrl-C cancels, Ctrl-H deletes backward, Ctrl-W deletes a word, Ctrl-U deletes back to the start, Alt-Enter inserts a newline, and Ctrl-S also applies. `:q` and `:help` are built-in commands available from every view; other command-prompt input is passed to jj. Descriptions and paths are passed as separate arguments, not interpolated into a shell. Only `$` deliberately runs a shell. File operations use exact, quoted workspace-relative filesets. Mutation sources use change IDs so Jujutsu's own pre-command snapshot can rewrite the working-copy commit safely.

Rebase currently moves the selected revisions onto the destination; squash moves their changes into it and retains the destination description. Git push publishes bookmarks associated with the captured source revisions. Select the desired revision before opening the Git view. Use the `:` command prompt for other supported jj command flags. The prompt rejects `log`, aliases, and repository/config overrides; shell commands are explicitly user-directed.

`o` expands related operations below the focused commit and pushes older commits down. `O` opens the repository-wide operation graph. Both use jj-style operation metadata, colors, curved graph edges, and a bold green `@` for the current operation. Related operations are those that change the focused change's visible commit versions, bookmark/tag references, or working-copy assignment; this follows rewrites and excludes unrelated edits. Hidden operations are contracted while retaining merge ancestry. Use `j`/`k` or the mouse to select an operation, `r` to restore it (with confirmation), and `h`/Left/Esc to close. Pressing `o` again collapses inline operations. On a snapshot operation, `d` compares its recorded working-copy trees against its actual parent operation, including added and deleted files. This shows only that snapshot’s changes, not the entire revision diff or current unsnapshotted edits. `h`/Left/Esc returns to the originating operation log and selection. Customize this key with the `diff` action in `[operations]`. These reads use `jj-lib`, without invoking `jj op log`. Bindings are configurable as `operations` and `full_operations` in `[revisions]` and `[operations]`.

Click a graph row or inline file to select it. Ctrl-click toggles revision selection. The wheel moves through revisions/files. External editors temporarily restore the normal terminal and refresh the TUI when they exit.

## Live updates and snapshots

`notify` watches the workspace and its shared repository. A background worker batches filesystem events over 75 ms, checks operation heads, and reloads only when needed. A two-second operation-head check recovers missed repository notifications. Successful explicit actions refresh the graph. Late diff results for another revision are discarded.

By default, browsing creates no operations and does not snapshot edits. Recent working-tree events appear in the header/help. Opt into automatic native snapshots with:

```sh
cargo run -- --auto-snapshot
```

This writes Jujutsu snapshot operations when files change, keeping `@` and its diff live without a `jj` subprocess. Idle snapshots create no operations. The working-copy lock, freshness checks, ignore patterns, `snapshot.auto-track`, `snapshot.max-new-file-size`, and immutable revision checks apply. Stale workspaces and divergent operation heads are reported rather than automatically reconciled. Run an appropriate `jj` command to resolve them. Snapshotting does not automatically import/export Git refs.

Library reads load user configuration (`JJ_CONFIG`, legacy/XDG paths), legacy and secure repository/workspace config (with path-identity verification), conditional scopes, and `JJ_USER`/`JJ_EMAIL`. Native config resolves command conditions as `jjui-rs`. Revset and fileset aliases are supported. Built-in fallback aliases for trunk/immutability conservatively include remote bookmarks and tags; user aliases override them. Explicit jj commands use Jujutsu's own full configuration/security handling. Restart to reload library settings. Native reads never migrate secure config; use `jj config path` to reconcile metadata after moving or copying a repository.

## Other entry points

```sh
cargo run -- --once
cargo run -- --watch-json
cargo run -- --diff FULL_COMMIT_ID
cargo run -- -r 'mutable()' --limit 500
cargo run -- --auto-snapshot --once
```

The default graph limit is 200 revisions. Quick search and parent/child jumps operate within that window. `L`/`-r` evaluates across the repository before applying the limit. `~` marks omitted ancestry; `╪` marks unrelated crossing edges.

Graph text follows jj's default compact log style: change ID, author email, local committer timestamp, bookmarks, and commit ID, followed by the description. IDs show at least eight characters, with repository-wide unique prefixes in bold magenta/blue and the remainder in gray. Emails are yellow, timestamps cyan, and working-copy text is bold with brighter colors. Empty changes, conflicts, missing descriptions, and the root commit have jj-style labels. This uses native metadata and fixed default styling; custom jj log templates and color overrides are not loaded.

## Current limits

This implements the core workflows above, not every jjui extension or advanced mode. Rebase insertion modes, annotation/range-diff views, Lua plugins, custom themes, and interactive chunk selection inside the TUI are not implemented. Interactive split/diff editing delegates to configured jj tools. The full operation log shows the newest 200 operations; the inline log scans history for up to 200 matching operations; command history lasts for the current session. There is no mouse drag/range selection.

Text diffs use whole-file unified hunks. Binary/non-UTF-8/conflicted files and files over 256 KiB are labeled rather than rendered as text. Previews stop after 200 files or about 2 MiB of patch output with a truncation notice; renames appear as delete/add. Background command output is limited to 2 MiB and commands time out after 120 seconds. Recursive filesystem watching can require a higher OS watch limit for large workspaces. The recent-event list is best-effort and excludes `.jj`, `.git`, and `target`; it is not a complete working-copy status.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build
python3 tests/pty_smoke.py  # Unix terminal integration test
```

Tests create temporary repositories and require `jj` 0.46 and Git. They cover graph topology, keybinding validation and remapping, inline details, prompts, mouse selection, native revsets/listings/snapshots, editing operations, local Git push/fetch, operation restore, and live updates with an empty viewer `PATH`.
