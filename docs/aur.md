# AUR packages

| Package       | Source                      | Rust needed by users |
| ------------- | --------------------------- | -------------------- |
| `jjui-rs`     | Versioned crates.io archive | Yes                  |
| `jjui-rs-bin` | GitHub release binaries     | No                   |
| `jjui-rs-git` | Latest `main`               | Yes                  |

All variants install `jjui-rs`, its licenses, and configuration examples. They conflict with one
another. Examples live under `/usr/share/jjui-rs`; packaging does not overwrite user configuration.

These recipes are prepared here; they are not available on AUR until a maintainer submits them.

## Release artifacts

The GitHub Actions release workflow builds native x86-64 and ARM64 Linux binaries on Ubuntu 24.04.
It packages the binary, licenses, docs, keybindings, themes, and examples. These GNU/Linux binaries
require glibc 2.39 or newer; they are not static musl builds.

Publish the crate before pushing its version tag. The workflow downloads that crate to compute the
source checksum and renders all three recipes from `packaging/aur/*.PKGBUILD.in`. It uses actual
binary archive checksums and `makepkg --printsrcinfo` to produce `.SRCINFO` files. Only the moving
Git source uses `SKIP` for its checksum.

A GitHub release contains both binary archives, `SHA256SUMS`, and `jjui-rs-VERSION-aur.tar.gz` with
ready-to-review AUR directories. The workflow needs the usual GitHub Actions token with permission
to write releases; it does not need an AUR credential or publish to crates.io.

For the existing `0.47.0` tag, first land the workflow on `main`, then dispatch it with
`tag=0.47.0`. It checks out the existing tag without changing it. Published assets are not
overwritten on reruns; if an upload partially succeeds, download the original workflow artifacts and
upload only missing files. Do not rebuild and replace binaries after their checksums have been
submitted to AUR.

## Generate recipes locally

Python 3.11+ and Arch's `makepkg` are required. Run as a regular user.

```sh
python3 scripts/prepare-aur.py --version 0.47.0 --assets dist --output target/aur
```

Put the two release binary archives in `dist` first. The script downloads the published crate;
`--crate PATH` allows an existing copy. With `--source-only`, it generates the stable-source and Git
recipes before binaries are available.

Source packages require Rust 1.97.1+ and build from the committed lockfile. Dependency fetching
happens in `prepare()`; builds and tests use `--frozen`. Rebuilds therefore do not automatically
update `jj-lib` like an unlocked `cargo install` would. Update the published crate or deliberately
revise the recipe and `pkgrel` when changing dependencies.

The runtime dependency is Arch's `jujutsu` package in the matching major/minor series. For `0.47`,
that means `jujutsu>=0.47` and `jujutsu<0.48`. Review these bounds and the Rust minimum when moving
to a new release series, including in the Git package. The `-git` recipe computes its version from
Git tags and commits; it must be rebuilt to pick up new commits.

## Validate and submit

For each generated directory, review the PKGBUILD, then build it in an Arch clean chroot and run
`namcap` on the recipe and resulting package. The source recipes run the Rust tests in `check()`.
Test installing each variant separately and confirm `jjui-rs --version` and opening a jj workspace.

Create an AUR account, register an SSH public key, and check package-name availability before the
initial submission. Each variant has its own AUR Git repository:

```sh
git clone ssh://aur@aur.archlinux.org/jjui-rs.git
```

Copy the generated `PKGBUILD` and `.SRCINFO` into that checkout, review the diff, commit, and push.
Repeat for `jjui-rs-bin` and `jjui-rs-git`. Add a maintainer contact to the recipes before
submitting. Do not upload binary archives to AUR.

On later releases, update the stable and binary recipes, regenerate `.SRCINFO`, and push their AUR
repositories. Refresh the Git recipe when build requirements or packaging change. Increase `pkgrel`
for packaging-only changes to an existing release; reset it to 1 for a new upstream version.

References: [AUR submission guidelines](https://wiki.archlinux.org/title/AUR_submission_guidelines),
[Rust packaging](https://wiki.archlinux.org/title/Rust_package_guidelines), and
[VCS packaging](https://wiki.archlinux.org/title/VCS_package_guidelines).
