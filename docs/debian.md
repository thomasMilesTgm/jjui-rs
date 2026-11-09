# Debian and Ubuntu packages

The release workflow builds `.deb` packages for `amd64` and `arm64`. The current binaries support
Ubuntu 24.04+ and Debian 13+; older systems such as Debian 12 have an incompatible glibc. Library
requirements are calculated by `dpkg-shlibdeps` on the build runner.

The packaging and signing workflows are prepared. The instructions below become usable once they are
merged and the initial Debian packaging and APT deployment workflows have completed.

## Download a package

Download the appropriate `.deb` and checksum manifest from
[GitHub Releases](https://github.com/thomasMilesTgm/jjui-rs/releases). Then:

```sh
sudo apt install ./jjui-rs_0.47.0-1_amd64.deb
```

Use the `arm64` file on ARM machines. A local `.deb` installation does not enable automatic updates.

Install a compatible `jj` CLI separately for editing and Git commands. For this release series:

```sh
cargo install jj-cli --version '~0.47' --locked
```

A distro-provided `jujutsu` package is also suitable if it supplies `jj` 0.47.x. The `.deb` suggests
that package rather than requiring it, so a Cargo-installed CLI can be used. Native graph reads do
not require the CLI. The header indicates missing or mismatched CLI versions.

## Enable APT updates

The repository is hosted at `https://thomasmilestgm.github.io/jjui-rs/`. It contains only jjui-rs;
it does not replace the distro's Jujutsu package or install a separate CLI.

Download the public key and verify its fingerprint:

```sh
curl -fsSLo /tmp/jjui-rs-archive-keyring.gpg \
  https://thomasmilestgm.github.io/jjui-rs/jjui-rs-archive-keyring.gpg
gpg --show-keys --with-fingerprint /tmp/jjui-rs-archive-keyring.gpg
```

Expected fingerprint: `8A3A F4CD 5680 0AA0 9ADF 63D6 A82B C9F2 7936 4A05`.

```sh
sudo install -Dm644 /tmp/jjui-rs-archive-keyring.gpg \
  /etc/apt/keyrings/jjui-rs-archive-keyring.gpg
sudo tee /etc/apt/sources.list.d/jjui-rs.sources >/dev/null <<'EOF'
Types: deb
URIs: https://thomasmilestgm.github.io/jjui-rs/
Suites: stable
Components: main
Architectures: amd64 arm64
Signed-By: /etc/apt/keyrings/jjui-rs-archive-keyring.gpg
EOF
sudo apt update
sudo apt install jjui-rs
```

Subsequent versions arrive through `sudo apt upgrade`. Here, `stable` is our release channel, not a
Debian distribution codename. A major/minor upgrade of jjui-rs may also require updating your
separately installed `jj` CLI.

The public signing key is also tracked in `packaging/apt/jjui-rs-archive-keyring.asc`. The key
applies only to this repository via `Signed-By`; do not add it to APT's global trusted keyring.

## Release automation

- New releases: the existing release workflow builds `.deb` files alongside the Linux archives.
- Existing releases: manually run **Package existing Debian release** with `tag=0.47.0` and
  `revision=1`. This downloads the original archives, verifies their checksums, and attaches `.deb`
  files plus `DEB-1-SHA256SUMS`. It never replaces the existing binaries or AUR checksums.
- **Publish signed APT repository** runs after either workflow succeeds, or by manual dispatch. It
  fetches and verifies packages from all retained stable GitHub releases, builds indices, signs
  them, and deploys to GitHub Pages. Both architectures and older package versions are retained.
- Increase the Debian revision when changing packaging for an existing app version. Reset it to 1
  for a new app release. Release assets are never overwritten.

APT `InRelease` and detached `Release.gpg` signatures authenticate the package indices, whose hashes
cover the `.deb` files. The repository also publishes compressed indices and SHA-256 by-hash paths.

GitHub Pages must use **GitHub Actions** as its publishing source. The deployment uses the
`github-pages` environment and needs `pages: write` and `id-token: write` permissions.

### Signing key

The dedicated project key expires on 2028-11-08. GitHub Actions uses:

| Setting                   | Kind     | Value                                      |
| ------------------------- | -------- | ------------------------------------------ |
| `APT_SIGNING_KEY`         | Secret   | ASCII-armored private signing key          |
| `APT_SIGNING_FINGERPRINT` | Variable | `8A3AF4CD56800AA09ADF63D6A82BC9F279364A05` |

A private backup and revocation certificate are stored locally at
`~/.local/share/jjui-rs/release-signing/`. Back up that directory securely; it is intentionally
outside Git. Do not use the AUR SSH key for APT signing.

Before key expiry, extend the existing key's expiry and update the GitHub secret and tracked public
key together. The workflow verifies that the configured secret exports exactly the tracked public
key. Key replacement requires distributing the new public key to existing clients before switching
signatures. Do not delete old GitHub package assets while they are still served to APT clients.

### Local validation

Use a disposable Ubuntu 24.04 container, with the source checkout and the original x86-64 release
archive mounted inside:

```sh
bash /source/tests/debian.sh /assets/jjui-rs-0.47.0-x86_64-unknown-linux-gnu.tar.gz
```

The test builds and installs the `.deb`, creates a temporary signing key, installs through a signed
local APT repository, and confirms that a tampered signature is rejected. Test signing keys are
separate from the production key.
