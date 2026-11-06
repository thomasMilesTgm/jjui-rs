# Publishing jjui-rs

## Verify

- Check the version in `Cargo.toml` and that it has not already been published.
- Keep `rust-version` and the README's Rust requirement consistent with the dependencies.
- Install `jj` 0.46 for the integration and terminal tests.

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked
python3 tests/pty_smoke.py
cargo package --list
cargo publish --dry-run --locked
```

The package includes embedded default bindings, presets, themes, plugin examples, documentation,
tests, and both licenses. Local configuration, repository metadata, and build output are excluded.

For preparation on an uncommitted change, add `--allow-dirty` to the packaging or dry-run command.
Commit the reviewed release before the actual publication so the package records its source
revision.

## Publish

Authenticate with crates.io using Cargo's credential provider, then:

```sh
cargo publish --locked
```

Publication is a separate, explicit step. Confirm the published version installs with
`cargo install jjui-rs --version 0.1.2 --locked` and reports the expected version with
`jjui-rs --version`. Update the version in that command for later releases.
