#!/usr/bin/env python3
"""Create a signed, static APT repository from released jjui-rs Debian packages."""
import argparse
import gzip
import hashlib
from pathlib import Path
import re
import shutil
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--packages', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--signing-key', required=True, help='Full fingerprint in the current GNUPGHOME')
    args = parser.parse_args()
    if not re.fullmatch(r'[A-Fa-f0-9]{40,64}', args.signing_key):
        parser.error('use a full signing key fingerprint')
    if args.output.exists() and any(args.output.iterdir()):
        parser.error('output directory must be empty; rebuild from all retained release packages')
    packages = sorted(args.packages.rglob('*.deb'))
    if not packages:
        parser.error('no Debian packages found')
    args.output.mkdir(parents=True, exist_ok=True)
    output = args.output.resolve()
    pool = output / 'pool/main/j/jjui-rs'
    pool.mkdir(parents=True)
    for path in packages:
        fields = subprocess.check_output(
            ['dpkg-deb', '--show', '--showformat=${Package}\t${Version}\t${Architecture}', str(path)], text=True)
        name, version, architecture = fields.split('\t')
        if name != 'jjui-rs' or architecture not in ['amd64', 'arm64'] or not re.fullmatch(r'\d+\.\d+\.\d+-\d+', version):
            parser.error(f'unexpected package metadata in {path}: {fields}')
        destination = pool / f'{name}_{version}_{architecture}.deb'
        if destination.exists() and destination.read_bytes() != path.read_bytes():
            parser.error(f'conflicting bytes for {destination.name}')
        shutil.copyfile(path, destination)
    for architecture in ['amd64', 'arm64']:
        directory = output / f'dists/stable/main/binary-{architecture}'
        directory.mkdir(parents=True)
        index = subprocess.check_output(['apt-ftparchive', '--arch', architecture, 'packages', 'pool'], cwd=output)
        for name, data in [('Packages', index), ('Packages.gz', gzip.compress(index, mtime=0))]:
            (directory / name).write_bytes(data)
            hashed = directory / 'by-hash/SHA256' / hashlib.sha256(data).hexdigest()
            hashed.parent.mkdir(parents=True, exist_ok=True)
            hashed.write_bytes(data)
    release_dir = output / 'dists/stable'
    command = ['apt-ftparchive']
    for name, value in {'Origin': 'jjui-rs', 'Label': 'jjui-rs', 'Suite': 'stable', 'Codename': 'stable',
                        'Architectures': 'amd64 arm64', 'Components': 'main', 'Acquire-By-Hash': 'yes',
                        'Description': 'jjui-rs releases for Debian and Ubuntu'}.items():
        command.extend(['-o', f'APT::FTPArchive::Release::{name}={value}'])
    release = subprocess.check_output(command + ['release', 'dists/stable'], cwd=output)
    (release_dir / 'Release').write_bytes(release)
    for filename, options in [('InRelease', ['--clearsign']), ('Release.gpg', ['--armor', '--detach-sign'])]:
        subprocess.run(['gpg', '--batch', '--yes', '--local-user', args.signing_key,
                        '--digest-algo', 'SHA256', '--output', str(release_dir / filename),
                        *options, str(release_dir / 'Release')], check=True)
    public_key = subprocess.check_output(['gpg', '--batch', '--export', args.signing_key])
    if not public_key:
        parser.error('could not export signing key')
    (output / 'jjui-rs-archive-keyring.gpg').write_bytes(public_key)
    (output / 'index.html').write_text(
        '<!doctype html><title>jjui-rs APT repository</title><h1>jjui-rs APT repository</h1>'
        '<p>Packages for amd64 and arm64, Ubuntu 24.04+ and Debian 13+.</p>'
        '<p><a href="https://github.com/thomasMilesTgm/jjui-rs/blob/main/docs/debian.md">'
        'Installation instructions</a></p>'
        f'<p>Signing key fingerprint: <code>{args.signing_key.upper()}</code></p>')
    print(output)


if __name__ == '__main__':
    main()
