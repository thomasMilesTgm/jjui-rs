#!/bin/bash
set -euo pipefail
# This integration test installs/removes packages and must run in a disposable container.
if [[ ! -f /.dockerenv && ! -f /run/.containerenv ]]; then
  echo 'Run this test inside a disposable Ubuntu 24.04 container.' >&2
  exit 1
fi
archive=$(realpath "${1:?Pass the x86-64 release archive}")
source_root=$(cd "$(dirname "$0")/.." && pwd)
test_root=$(mktemp -d)
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends python3 dpkg-dev binutils apt-utils gnupg ca-certificates curl
python3 "$source_root"/scripts/package-deb.py --version "${2:-0.47.0}" --architecture amd64 --archive "$archive" --output "$test_root"/dist
apt-get install -y "$test_root"/dist/*.deb
jjui-rs --version
jjui-rs --check-keybinds > "$test_root"/keybinds.txt
jjui-rs --check-theme > "$test_root"/theme.txt
export GNUPGHOME="$test_root/test-gnupg"
mkdir -m700 "$GNUPGHOME"
gpg --batch --pinentry-mode loopback --passphrase '' --quick-generate-key 'APT test <test@example.invalid>' rsa2048 sign 1d
fingerprint=$(gpg --batch --with-colons --list-secret-keys | awk -F: '$1=="fpr" {print $10; exit}')
python3 "$source_root"/scripts/build-apt-repo.py --packages "$test_root"/dist --output "$test_root"/site --signing-key "$fingerprint"
gpg --verify "$test_root"/site/dists/stable/InRelease
chmod 755 "$test_root"
install -m644 "$test_root"/site/jjui-rs-archive-keyring.gpg /usr/share/keyrings/jjui-test.gpg
cat > /etc/apt/sources.list.d/jjui-test.sources <<EOF
Types: deb
URIs: file:$test_root/site
Suites: stable
Components: main
Signed-By: /usr/share/keyrings/jjui-test.gpg
EOF
apt-get update
apt-cache policy jjui-rs
apt-get remove -y jjui-rs
apt-get install -y jjui-rs
jjui-rs --version
# A modified signed manifest must fail verification.
sed -i 's/Label: jjui-rs/Label: altered/' "$test_root"/site/dists/stable/InRelease
if apt-get update -o APT::Update::Error-Mode=any; then
  echo 'ERROR: APT accepted a modified signed manifest' >&2
  exit 1
fi
printf 'Debian package and signed APT installation checks passed\n'
