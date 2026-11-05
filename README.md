# jjui-rs

An early Rust terminal browser for Jujutsu, inspired by [jjui](https://github.com/idursun/jjui). Live updates are always enabled. The application reads repositories through `jj-lib`; it never launches `jj`, `jj log`, or another CLI subprocess.

```sh
cargo run --release -- -R /path/to/workspace
```

Requires Rust 1.97.1 or newer. `jj-lib` is pinned to 0.46.0, matching the Jujutsu version used for integration testing. Repository compatibility with other versions is not guaranteed. The `jj` executable is only needed to create/manage repositories and run integration tests.

## Current features

- Topologically ordered revision list, current-workspace marker, local bookmarks, parent IDs, author, and descriptions.
- Automatic refresh after external Jujutsu operations, including changes from other workspaces sharing the repository.
- Selection retained by commit ID, then change ID, across rewrites.
- Quick search across IDs, descriptions, and bookmark names, without hiding revisions.
- Recent filesystem events for unsnapshotted working-tree edits.
- JSON snapshot and streaming modes for scripts and testing.

Bindings follow [jjui's defaults](https://github.com/idursun/jjui/blob/main/internal/config/default/bindings.toml) for the browsing actions implemented here.

| Key | Action |
| --- | --- |
| `j` / `k`, arrows | Move selection |
| Page Up / Page Down | Move one visible page |
| Home / End | First / last revision |
| `@` | Jump to working copy |
| `J` / `K` | Jump to a loaded parent / child |
| `/` | Enter quick search; matching rows are highlighted |
| Enter | Finish entering search |
| `'` / `"` | Next / previous match, wrapping at the ends |
| Esc | Clear search and notices, keeping cursor position |
| Ctrl-R | Force reload |
| `q`, Ctrl-C | Quit |

Plain `r` and `g` are reserved for jjui's rebase and Git actions and show an unsupported-action notice. They no longer refresh or jump to the first revision. `G` is no longer a last-revision shortcut. Other jjui actions (including preview toggling, file details, multi-selection, and mutation commands) are not yet implemented. Ctrl-C remains an application-level quit shortcut.

```sh
cargo run -- --once
cargo run -- --watch-json
cargo run -- -R ../another-workspace --limit 500
```

The default limit is 200 revisions. Search and parent/child jumps apply to this loaded window, not to the entire repository. When multiple parents or children are loaded, the first in display order is used.

## How live updates work

`notify` watches the workspace and, when separate, its shared repository. A background worker batches events over 75 milliseconds. It checks operation heads and reloads revisions only when the operation changes. A two-second operation-head check recovers repository updates missed by the filesystem watcher. Repository loading stays off the UI thread.

The viewer loads a specific operation rather than using `load_at_head()`, which can automatically merge concurrent operations. It creates no transactions, snapshots, or working-copy locks. Jujutsu may still populate internal read caches/indexes. If operation heads diverge, the viewer reports the problem and retains its last successful view; run a normal `jj` command to reconcile them.

Unsnapshotted edits appear as **recent file events**, not a computed diff or working-copy status. They do not rewrite `@`. Revision contents reflect Jujutsu's latest recorded snapshot. File events exclude `.jj`, `.git`, and `target` paths; other ignore rules are not yet applied. The event list is bounded and best-effort, and missed file events are not reconstructed by the operation-head fallback.

## Scope

This is a browsing foundation, not feature parity with jjui. Graph edges, diff previews, arbitrary revsets, mutation commands, full jj configuration loading, and automatic working-copy snapshotting are not implemented. It uses default Jujutsu backends and library settings. Recursive filesystem watching may need a higher OS watch limit for large workspaces; startup reports watcher failures.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Integration tests require `jj` 0.46 on `PATH`. They create temporary repositories, remove `jj` from the viewer's environment, and check live external operations, unsnapshotted edits without operation writes, and shared-workspace updates.
