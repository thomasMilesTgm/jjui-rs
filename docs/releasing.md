# Publishing jjui-rs

The major and minor version follow `jj-lib`; the patch version counts jjui-rs releases for that
Jujutsu version. Keep the dependency requirement at `jj-lib = "0.47"` for this release series so
users can pick up any compatible patch.

Normal `cargo install jjui-rs` resolves compatible dependency updates. Reinstalling the same app
version requires `--force`. Installs with `--locked` use our published lockfile, so update that file
and publish a new jjui-rs patch to deliver dependency updates to those users. Validate releases with
`--locked` so the packaged dependency versions are tested.

## Verify

- Check the version in `Cargo.toml` and that it has not already been published.
- Keep `rust-version` and the README's Rust requirement consistent with the dependencies.
- Install `jj` 0.47 for the integration and terminal tests.

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
`cargo install jjui-rs --version 0.47.0 --locked` and reports the expected version with
`jjui-rs --version`. Update the version in that command for later releases.
